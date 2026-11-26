//! `weir watch`: follow a sync that some other process is running.

use anyhow::Result;
use rusqlite::Connection;
use std::io::{IsTerminal, Write};
use std::time::Duration;

use crate::runs::{self, RunRow};
use crate::sync;
use crate::util;

fn elapsed_since(started_at: &str) -> u64 {
    util::parse_utc(started_at)
        .map(|t| (util::unix_now() - t).max(0) as u64)
        .unwrap_or(0)
}

/// The one-line picture of a run in progress, as `weir status` shows it.
pub fn progress(conn: &Connection, run: &RunRow) -> Result<String> {
    let phase = match run.phase.as_deref() {
        Some("listing") | None => "listing repositories".to_string(),
        Some(other) => other.to_string(),
    };
    Ok(sync::status_line(
        &phase,
        run.done as usize,
        run.planned.unwrap_or(0) as usize,
        &runs::live_counts(conn, run)?,
        elapsed_since(&run.started_at),
        &run.current(),
    ))
}

/// The run that is in progress right now, if there is one.
pub fn running(conn: &Connection) -> Result<Option<RunRow>> {
    if !sync::is_running() {
        return Ok(None);
    }
    Ok(runs::latest(conn, 1)?
        .pop()
        .filter(|r| r.finished_at.is_none()))
}

pub fn watch(conn: &Connection, follow: bool) -> Result<i32> {
    let tty = std::io::stderr().is_terminal();
    let mut watching: Option<i64> = None;
    let mut seen = 0;
    let mut drawn = false;
    let clear = |drawn: &mut bool| {
        if *drawn {
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
            *drawn = false;
        }
    };
    loop {
        let live = running(conn)?;
        // Print whatever the watched run has recorded since the last look.
        if let Some(id) = watching.or(live.as_ref().map(|r| r.id)) {
            if watching != Some(id) {
                watching = Some(id);
                seen = 0;
            }
            for (event_id, e) in runs::events(conn, id, seen)? {
                seen = event_id;
                if e.action != "unchanged" {
                    clear(&mut drawn);
                    println!("{}", sync::describe(&e));
                }
            }
        }
        match (&live, watching) {
            (Some(run), _) => {
                if tty {
                    eprint!("\r\x1b[2K{}", sync::fit(&progress(conn, run)?));
                    let _ = std::io::stderr().flush();
                    drawn = true;
                }
            }
            (None, Some(id)) => {
                // The run we were watching has ended: say how.
                clear(&mut drawn);
                let run = runs::latest(conn, 20)?.into_iter().find(|r| r.id == id);
                let mut code = 0;
                if let Some(run) = run {
                    for p in run.problems() {
                        eprintln!("problem   {p}");
                    }
                    match run.verdict() {
                        "ok" | "problems" => println!(
                            "weir: {} in {}s",
                            sync::summarize(&run.counts()),
                            run.seconds.unwrap_or(0)
                        ),
                        other => println!("weir: the sync was {other}"),
                    }
                    if run.verdict() != "ok" {
                        code = 1;
                    }
                }
                watching = None;
                if !follow {
                    return Ok(code);
                }
            }
            (None, None) => {
                if !follow {
                    println!("No sync is running.");
                    return Ok(0);
                }
                if tty && !drawn {
                    eprint!("\r\x1b[2Kwaiting for the next sync…");
                    let _ = std::io::stderr().flush();
                    drawn = true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
