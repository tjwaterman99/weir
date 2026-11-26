//! `weir daemon`: the sync engine on a loop, so mirrors stay minutes behind
//! their providers instead of a day.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::{db, provider, settings, sync, util};

pub struct Opts {
    /// Seconds between cycles; the `interval_seconds` setting when absent.
    pub interval: Option<u64>,
    /// Stop after this many cycles. For tests and one-off runs.
    pub cycles: Option<u64>,
}

fn lock_file() -> Result<File> {
    let path = db::home().join("daemon.lock");
    File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("could not open {}", path.display()))
}

/// Whether a daemon is alive right now. It holds a lock for its whole life,
/// and the operating system releases it however the daemon ends.
pub fn is_running() -> bool {
    let Ok(file) = lock_file() else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
    }
}

fn note(text: &str) {
    eprintln!("{}  {text}", util::now());
}

fn set_state(conn: &Connection, state: &str, message: Option<&str>, next: Option<i64>) {
    let _ = conn.execute(
        "UPDATE daemon SET state = ?1, message = ?2, next_cycle_at = ?3 WHERE id = 1",
        params![state, message, next.map(util::fmt_utc)],
    );
}

/// Waits longer after each failure in a row, up to half an hour, so a broken
/// provider is not hammered.
pub fn backoff(interval: u64, failures: u32) -> u64 {
    interval
        .saturating_mul(1 << failures.min(6))
        .clamp(interval, 1_800.max(interval))
}

enum Nap {
    Done,
    /// The clock jumped: the machine was asleep. Catch up now.
    Woke,
    Stop,
}

fn nap(secs: u64, stop: &AtomicBool) -> Nap {
    let tick = Duration::from_millis(500);
    let deadline = SystemTime::now() + Duration::from_secs(secs);
    let mut last = SystemTime::now();
    loop {
        std::thread::sleep(tick);
        if stop.load(Ordering::Relaxed) {
            return Nap::Stop;
        }
        let now = SystemTime::now();
        // Half a second of sleep that took half a minute means the machine slept.
        if now
            .duration_since(last)
            .is_ok_and(|gap| gap > Duration::from_secs(30))
        {
            return Nap::Woke;
        }
        last = now;
        if now >= deadline {
            return Nap::Done;
        }
    }
}

/// Whether at least one tracked provider can be reached. A provider that
/// needs no network (a local one) always can.
fn online(conn: &Connection) -> Result<bool> {
    let providers = db::providers(conn)?;
    let mut endpoints = Vec::new();
    for t in db::tracked(conn)? {
        let Some(p) = providers.iter().find(|p| p.id == t.provider_id) else {
            continue;
        };
        match provider::build(p).ok().and_then(|prov| prov.endpoint()) {
            Some(endpoint) if !endpoints.contains(&endpoint) => endpoints.push(endpoint),
            Some(_) => {}
            None => return Ok(true),
        }
    }
    Ok(endpoints.is_empty() || endpoints.iter().any(|e| sync::reachable(e)))
}

/// Whether the machine is running on its battery.
pub fn on_battery() -> bool {
    if cfg!(target_os = "macos") {
        std::process::Command::new("pmset")
            .args(["-g", "batt"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("Battery Power"))
    } else {
        std::fs::read_dir("/sys/class/power_supply")
            .map(|dir| {
                dir.flatten().any(|supply| {
                    std::fs::read_to_string(supply.path().join("status"))
                        .is_ok_and(|s| s.trim() == "Discharging")
                })
            })
            .unwrap_or(false)
    }
}

pub fn run(conn: &Connection, o: &Opts) -> Result<i32> {
    let lock = lock_file()?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => bail!("the weir daemon is already running"),
        Err(std::fs::TryLockError::Error(e)) => {
            return Err(e).context("could not take the daemon lock")
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed))
            .context("could not install the signal handler")?;
    }
    conn.execute(
        "INSERT INTO daemon (id, pid, started_at, state) VALUES (1, ?1, ?2, 'starting')
         ON CONFLICT (id) DO UPDATE SET
           pid = excluded.pid, started_at = excluded.started_at, state = 'starting',
           last_cycle_at = NULL, next_cycle_at = NULL, message = NULL",
        params![std::process::id(), util::now()],
    )?;
    note("weir daemon started");

    let mut failures = 0u32;
    let mut cycles = 0u64;
    let mut was_offline = false;
    let mut was_paused = false;
    while !stop.load(Ordering::Relaxed) {
        // Settings are read every cycle, so changing one needs no restart.
        let interval = match o.interval {
            Some(secs) => secs,
            None => settings::number(conn, "interval_seconds")?,
        }
        .max(1);
        if settings::get(conn, "pause_on_battery")? == "true" && on_battery() {
            if !std::mem::replace(&mut was_paused, true) {
                note("on battery power; pausing");
            }
            set_state(conn, "paused", Some("on battery power"), None);
            if matches!(nap(30, &stop), Nap::Stop) {
                break;
            }
            continue;
        }
        was_paused = false;
        if db::tracked(conn)?.is_empty() {
            // Nothing to do yet is not a failure. Look again soon, so the
            // first `weir track` is picked up quickly.
            set_state(conn, "idle", Some("nothing is tracked yet"), None);
            if matches!(nap(interval.min(30), &stop), Nap::Stop) {
                break;
            }
            continue;
        }
        if !online(conn)? {
            // Offline is a state, not an error: wait quietly, then catch up.
            if !std::mem::replace(&mut was_offline, true) {
                note("the providers cannot be reached; waiting for the network");
            }
            set_state(conn, "offline", Some("waiting for the network"), None);
            if matches!(nap(15, &stop), Nap::Stop) {
                break;
            }
            continue;
        }
        if std::mem::take(&mut was_offline) {
            note("back online");
        }

        set_state(conn, "syncing", None, None);
        let report = sync::run_report(
            conn,
            &sync::Opts {
                target: None,
                dry_run: false,
                all: false,
                jobs: settings::number(conn, "daemon_jobs")?.max(1) as usize,
                network_wait: 0,
                json: false,
                quiet: true,
                wait: true,
                daemon: true,
                stop: Some(stop.clone()),
            },
        );
        cycles += 1;
        let now = util::unix_now();
        let mut pause = interval;
        let mut message = None;
        match report {
            Ok(r) if r.stopped => break,
            Ok(r) => {
                failures = if r.ok { 0 } else { failures + 1 };
                if let Some(reset) = r.rate_reset.filter(|reset| *reset > now) {
                    pause = pause.max((reset - now) as u64 + 5);
                    message = Some("waiting for the rate limit to reset".to_string());
                } else if !r.ok {
                    pause = backoff(interval, failures);
                    message = Some("the last cycle had problems; see `weir status`".to_string());
                }
            }
            Err(e) => {
                failures += 1;
                pause = backoff(interval, failures);
                note(&format!("cycle failed: {e:#}"));
                message = Some(format!("{e:#}"));
            }
        }
        let _ = conn.execute(
            "UPDATE daemon SET last_cycle_at = ?1 WHERE id = 1",
            [util::fmt_utc(now)],
        );
        if o.cycles.is_some_and(|n| cycles >= n) {
            break;
        }
        set_state(conn, "idle", message.as_deref(), Some(now + pause as i64));
        match nap(pause, &stop) {
            Nap::Stop => break,
            Nap::Woke => note("the machine woke up; syncing now"),
            Nap::Done => {}
        }
    }
    let _ = conn.execute(
        "UPDATE daemon SET state = 'stopped', pid = NULL, next_cycle_at = NULL, message = NULL
         WHERE id = 1",
        [],
    );
    note("weir daemon stopped");
    Ok(0)
}

/// One line about the daemon for `weir status`, when there is anything to say.
pub fn describe(conn: &Connection) -> Result<Option<String>> {
    if !is_running() {
        return Ok(None);
    }
    type Row = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let row: Option<Row> = conn
        .query_row(
            "SELECT state, last_cycle_at, next_cycle_at, message FROM daemon WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((state, last, next, message)) = row else {
        return Ok(Some("Daemon running".to_string()));
    };
    let now = util::unix_now();
    let mut line = format!("Daemon running: {}", state.as_deref().unwrap_or("starting"));
    if let Some(t) = last.as_deref().and_then(util::parse_utc) {
        line.push_str(&format!("; last cycle {} ago", util::ago(now - t)));
    }
    if let Some(t) = next.as_deref().and_then(util::parse_utc) {
        line.push_str(&format!("; next in {}", util::ago((t - now).max(0))));
    }
    if let Some(m) = message {
        line.push_str(&format!(" ({m})"));
    }
    Ok(Some(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backs_off_after_failures_and_never_below_the_interval() {
        assert_eq!(backoff(300, 0), 300);
        assert_eq!(backoff(300, 1), 600);
        assert_eq!(backoff(300, 2), 1_200);
        assert_eq!(backoff(300, 3), 1_800);
        assert_eq!(backoff(300, 30), 1_800);
        assert_eq!(backoff(3_600, 4), 3_600);
    }
}
