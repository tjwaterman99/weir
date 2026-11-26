//! The sync engine: ask each provider what exists, compare with the database,
//! then clone, fetch, move or repair mirrors until the disk matches.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{IsTerminal, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use crate::db::{self, ProviderRow, RepoRow, TrackedRow};
use crate::git::{self, Job, Outcome};
use crate::provider::{self, AuthError, Cache, Lookup, RateLimit, RemoteRepo};
use crate::secret::Source;
use crate::settings;
use crate::stray;
use crate::target;
use crate::util;

pub struct Opts {
    pub target: Option<String>,
    pub dry_run: bool,
    pub all: bool,
    pub jobs: usize,
    pub network_wait: u64,
    pub json: bool,
    pub quiet: bool,
    /// Wait for a sync that is already running instead of giving up.
    pub wait: bool,
    /// Running as a cycle of `weir daemon`: stay quiet, and do not let a
    /// history of idle cycles pile up.
    pub daemon: bool,
    /// Set from outside to stop early: no new work is started, clones and
    /// fetches in flight are stopped, checkouts are allowed to finish.
    pub stop: Option<Arc<AtomicBool>>,
}

/// How a run ended, for callers that need more than an exit code.
pub struct Report {
    pub code: i32,
    pub ok: bool,
    /// It was stopped early through `Opts::stop`.
    pub stopped: bool,
    /// When a provider's rate limit resets, if this run ran into it.
    pub rate_reset: Option<i64>,
}

/// One line of sync output: something that happened to one repository.
#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub action: String,
    pub repo: String,
    #[serde(skip)]
    pub repo_id: Option<i64>,
    pub old_head: Option<String>,
    pub new_head: Option<String>,
    pub message: Option<String>,
    /// How long the work on this repository took.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub millis: Option<i64>,
}

struct Plan {
    jobs: Vec<Job>,
    /// Changes found while listing: repositories that are gone or filtered out.
    events: Vec<Event>,
    problems: Vec<String>,
    unchanged: usize,
    rate_reset: Option<i64>,
}

/// Below this many requests left, weir stops listing until the limit resets.
const RATE_FLOOR: u64 = 25;

/// How many past runs are kept for `weir log`.
const RUNS_KEPT: i64 = 500;

/// Runs a sync and returns the process exit code.
pub fn run(conn: &Connection, o: &Opts) -> Result<i32> {
    Ok(run_report(conn, o)?.code)
}

pub fn run_report(conn: &Connection, o: &Opts) -> Result<Report> {
    let root = settings::root(conn)?;
    let _lock = if o.dry_run { None } else { Some(lock(o.wait)?) };
    let stopping = || o.stop.as_ref().is_some_and(|s| s.load(Ordering::Relaxed));
    let started = Instant::now();

    let run_id = if o.dry_run {
        None
    } else {
        // The lock is ours, so a run that never finished is one that died.
        conn.execute(
            "UPDATE sync_runs SET finished_at = ?1, phase = 'interrupted', ok = 0, current = NULL
             WHERE finished_at IS NULL",
            [util::now()],
        )?;
        conn.execute(
            "DELETE FROM sync_runs WHERE id <= (SELECT MAX(id) FROM sync_runs) - ?1",
            [RUNS_KEPT],
        )?;
        conn.execute(
            "INSERT INTO sync_runs (started_at, phase, pid) VALUES (?1, 'listing', ?2)",
            params![util::now(), std::process::id()],
        )?;
        Some(conn.last_insert_rowid())
    };
    let mut ui = Ui::new(o, run_id);
    ui.begin();

    // Planning writes what the providers report into the database. A dry run
    // does it inside a transaction and rolls it back, so it leaves no trace.
    if o.dry_run {
        conn.execute_batch("BEGIN IMMEDIATE")?;
    }
    let planned = plan(conn, o, &root, &mut ui);
    if o.dry_run {
        let _ = conn.execute_batch("ROLLBACK");
    }
    let plan = match planned {
        Ok(p) => p,
        Err(e) => {
            ui.clear();
            if let Some(id) = run_id {
                conn.execute(
                    "UPDATE sync_runs SET finished_at = ?1, seconds = ?2, phase = 'failed', ok = 0,
                            summary = ?3
                     WHERE id = ?4",
                    params![
                        util::now(),
                        started.elapsed().as_secs() as i64,
                        json!({ "problems": [format!("{e:#}")] }).to_string(),
                        id
                    ],
                )?;
            }
            return Err(e);
        }
    };
    let listing_seconds = started.elapsed().as_secs() as i64;
    ui.planned(plan.jobs.len(), plan.unchanged);

    let Some(run_id) = run_id else {
        for e in &plan.events {
            ui.event(e);
        }
        for job in &plan.jobs {
            let action = if !job.path.exists() && job.move_from.is_none() {
                "clone"
            } else if job.move_from.is_some() {
                "move"
            } else if job.fetch {
                "fetch"
            } else {
                "heal"
            };
            ui.done += 1;
            ui.event(&Event {
                action: action.to_string(),
                repo: job.repo.clone(),
                repo_id: Some(job.repo_id),
                old_head: None,
                new_head: None,
                message: None,
                millis: None,
            });
        }
        ui.finish(&plan.problems, true);
        return Ok(Report {
            code: if plan.problems.is_empty() { 0 } else { 1 },
            ok: plan.problems.is_empty(),
            stopped: false,
            rate_reset: plan.rate_reset,
        });
    };

    conn.execute(
        "UPDATE sync_runs SET planned = ?1, unchanged = ?2, phase = 'syncing', listing_seconds = ?3
         WHERE id = ?4",
        params![
            plan.jobs.len() as i64,
            plan.unchanged as i64,
            listing_seconds,
            run_id
        ],
    )?;
    for e in &plan.events {
        record_event(conn, run_id, e)?;
        ui.event(e);
    }

    let pushed: HashMap<i64, (Option<String>, String, String, String)> =
        db::repos(conn, "state IN ('active', 'archived')", &[])?
            .into_iter()
            .map(|r| (r.id, (r.pushed_at, r.default_branch, r.owner, r.name)))
            .collect();

    enum Msg {
        Started(String),
        Done(Box<Job>, Outcome, i64),
    }
    let queue = Mutex::new(plan.jobs.into_iter());
    let (tx, rx) = mpsc::channel();
    let workers = o.jobs.max(1);
    let mut failure: Option<anyhow::Error> = None;
    let mut aborted = false;
    let planned_jobs = ui.total;
    let planned_events = plan.events.len();
    std::thread::scope(|s| {
        for _ in 0..workers {
            let tx = tx.clone();
            let queue = &queue;
            let stop = o.stop.clone();
            s.spawn(move || loop {
                if stop.as_ref().is_some_and(|s| s.load(Ordering::Relaxed)) {
                    break;
                }
                let job = queue.lock().unwrap().next();
                let Some(job) = job else { break };
                let label = match job.size_kb {
                    // Say how big a slow clone is, so it does not look stuck.
                    Some(kb) if kb >= 512_000 && !job.path.exists() => {
                        format!("{} ({})", job.repo, util::size(kb))
                    }
                    _ => job.repo.clone(),
                };
                let _ = tx.send(Msg::Started(label));
                let began = Instant::now();
                let outcome = git::sync_mirror(&job);
                let millis = began.elapsed().as_millis() as i64;
                if tx.send(Msg::Done(Box::new(job), outcome, millis)).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        loop {
            let (job, outcome, millis) = match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Msg::Started(label)) => {
                    ui.current.push((label, Instant::now()));
                    let _ = heartbeat(conn, run_id, &ui);
                    ui.draw();
                    continue;
                }
                Ok(Msg::Done(job, outcome, millis)) => (job, outcome, millis),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if stopping() && !aborted {
                        // Stop what can be stopped safely, once.
                        git::abort_network();
                        aborted = true;
                    }
                    ui.tick();
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            ui.current
                .retain(|(label, _)| !label.starts_with(job.repo.as_str()));
            if aborted && outcome.action == git::Action::Failed {
                // Stopped by us, not broken: leave its record as it was.
                continue;
            }
            let event = Event {
                action: outcome.action.as_str().to_string(),
                repo: job.repo.clone(),
                repo_id: Some(job.repo_id),
                old_head: outcome.old_head.clone(),
                new_head: outcome.new_head.clone(),
                message: outcome.message(),
                millis: Some(millis),
            };
            let stored = (|| -> Result<()> {
                let now = util::now();
                if outcome.action.is_problem() {
                    conn.execute(
                        "UPDATE repos SET last_synced_at = ?1, last_result = ?2, last_message = ?3
                         WHERE id = ?4",
                        params![now, event.action, event.message, job.repo_id],
                    )?;
                } else if let Some((pushed_at, branch, owner, name)) = pushed.get(&job.repo_id) {
                    conn.execute(
                        "UPDATE repos SET head = ?1, synced_pushed_at = ?2, synced_branch = ?3,
                                last_synced_at = ?4, last_result = 'ok', last_message = ?5,
                                disk_owner = ?6, disk_name = ?7
                         WHERE id = ?8",
                        params![
                            outcome.new_head,
                            pushed_at,
                            branch,
                            now,
                            event.message,
                            owner,
                            name,
                            job.repo_id
                        ],
                    )?;
                }
                // Unchanged repositories are recorded too: their timings are
                // what explains a slow sync.
                record_event(conn, run_id, &event)
            })();
            if let Err(e) = stored {
                failure.get_or_insert(e);
            }
            ui.done += 1;
            ui.event(&event);
            let _ = heartbeat(conn, run_id, &ui);
        }
    });
    if let Some(e) = failure {
        ui.clear();
        return Err(e.context("could not record sync results"));
    }

    let stopped = stopping();
    let bad = ui.count("failed") + ui.count("skipped");
    let ok = bad == 0 && plan.problems.is_empty() && !stopped;
    let idle = ok && planned_jobs == 0 && planned_events == 0;
    let mut summary = serde_json::to_value(&ui.counts)?;
    summary["problems"] = json!(plan.problems);
    // The run before this one, to tell a new problem from a continuing one.
    let previous_ok: Option<bool> = conn
        .query_row(
            "SELECT ok FROM sync_runs WHERE id < ?1 AND finished_at IS NOT NULL
             ORDER BY id DESC LIMIT 1",
            [run_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    conn.execute(
        "UPDATE sync_runs SET finished_at = ?1, seconds = ?2, summary = ?3, ok = ?4,
                phase = ?5, current = NULL, done = ?6, idle = ?7
         WHERE id = ?8",
        params![
            util::now(),
            started.elapsed().as_secs() as i64,
            summary.to_string(),
            ok,
            if stopped { "stopped" } else { "done" },
            ui.done as i64,
            idle,
            run_id
        ],
    )?;
    if o.daemon && idle {
        // A daemon cycle with nothing to do replaces the idle run before it,
        // so the history keeps what happened, not every time nothing did.
        conn.execute("DELETE FROM sync_runs WHERE idle = 1 AND id < ?1", [run_id])?;
    }
    ui.finish(&plan.problems, false);
    // A daemon reports a problem when it starts, not on every cycle it lasts.
    if !ok && !stopped && !(o.daemon && previous_ok == Some(false)) {
        on_problem(conn, run_id, &ui, &plan.problems);
    }
    Ok(Report {
        code: if ok { 0 } else { 1 },
        ok,
        stopped,
        rate_reset: plan.rate_reset,
    })
}

/// Writes how far the run has got, for `weir status` and `weir watch`.
fn heartbeat(conn: &Connection, run_id: i64, ui: &Ui) -> Result<()> {
    let now = Instant::now();
    let current: Vec<serde_json::Value> = ui
        .current
        .iter()
        .map(|(label, since)| json!({ "repo": label, "seconds": (now - *since).as_secs() }))
        .collect();
    conn.execute(
        "UPDATE sync_runs SET done = ?1, current = ?2 WHERE id = ?3",
        params![ui.done as i64, json!(current).to_string(), run_id],
    )?;
    Ok(())
}

/// Runs the user's `on_problem` command, if one is set. Its failure is
/// reported and otherwise ignored: it must never turn a sync into an error.
fn on_problem(conn: &Connection, run_id: i64, ui: &Ui, problems: &[String]) {
    let Ok(command) = settings::get(conn, "on_problem") else {
        return;
    };
    if command.trim().is_empty() {
        return;
    }
    let mut details: Vec<String> = problems.to_vec();
    details.extend(ui.trouble.iter().cloned());
    let summary = summarize(&ui.counts);
    let text = format!("weir sync: {summary}\n{}\n", details.join("\n"));
    let spawned = std::process::Command::new("sh")
        .arg("-c")
        .arg(&command)
        .env("WEIR_RUN_ID", run_id.to_string())
        .env("WEIR_SUMMARY", &summary)
        .env("WEIR_PROBLEMS", details.join("\n"))
        .stdin(std::process::Stdio::piped())
        .spawn();
    match spawned {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            match child.wait() {
                Ok(status) if status.success() => {}
                Ok(status) => eprintln!("weir: the on_problem command exited with {status}"),
                Err(e) => eprintln!("weir: the on_problem command failed: {e}"),
            }
        }
        Err(e) => eprintln!("weir: could not run the on_problem command: {e}"),
    }
}

fn record_event(conn: &Connection, run_id: i64, e: &Event) -> Result<()> {
    conn.execute(
        "INSERT INTO repo_syncs (run_id, repo_id, repo, action, old_head, new_head, message, millis)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            run_id, e.repo_id, e.repo, e.action, e.old_head, e.new_head, e.message, e.millis
        ],
    )?;
    Ok(())
}

/// Formats an event the way `sync`, `watch` and `log` all print it.
pub fn describe(e: &Event) -> String {
    let mut line = format!("{:<9} {}", e.action, e.repo);
    if let (Some(old), Some(new)) = (&e.old_head, &e.new_head) {
        if old != new {
            line.push_str(&format!("  {} -> {}", util::short(old), util::short(new)));
        }
    }
    if let Some(m) = &e.message {
        line.push_str(&format!("  ({m})"));
    }
    line
}

/// Counts as a sentence fragment: `3 cloned, 12 updated, 780 unchanged`.
pub fn summarize(counts: &BTreeMap<String, usize>) -> String {
    let order = [
        "clone",
        "fetch",
        "move",
        "heal",
        "cloned",
        "adopted",
        "updated",
        "healed",
        "moved",
        "archived",
        "recreated",
        "transferred",
        "inaccessible",
        "gone",
        "excluded",
        "skipped",
        "failed",
        "unchanged",
    ];
    let parts: Vec<String> = order
        .iter()
        .filter_map(|k| {
            counts
                .get(*k)
                .filter(|n| **n > 0)
                .map(|n| format!("{n} {k}"))
        })
        .collect();
    if parts.is_empty() {
        "nothing to do".to_string()
    } else {
        parts.join(", ")
    }
}

/// The status line shown while a sync runs: how far along, what so far, and
/// what is being worked on right now.
pub fn status_line(
    phase: &str,
    done: usize,
    total: usize,
    counts: &BTreeMap<String, usize>,
    elapsed_secs: u64,
    current: &[(String, u64)],
) -> String {
    let mut line = if total == 0 && done == 0 {
        phase.to_string()
    } else if counts.values().all(|n| *n == 0) {
        format!("[{done}/{total}]")
    } else {
        format!("[{done}/{total}] {}", summarize(counts))
    };
    line.push_str(&format!(" · {}", util::ago(elapsed_secs as i64)));
    if !current.is_empty() {
        let shown: Vec<String> = current
            .iter()
            .take(3)
            .map(|(label, secs)| {
                let name = label.rsplit_once('/').map(|x| x.1).unwrap_or(label);
                if *secs >= 10 {
                    format!("{name} {}", util::ago(*secs as i64))
                } else {
                    name.to_string()
                }
            })
            .collect();
        line.push_str(&format!(" · {}", shown.join(", ")));
        if current.len() > 3 {
            line.push_str(&format!(", +{}", current.len() - 3));
        }
    }
    line
}

/// Cuts a line to the terminal's width, so redrawing it never wraps.
pub fn fit(line: &str) -> String {
    let width = terminal_size::terminal_size_of(std::io::stderr())
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(100)
        .max(20);
    if line.chars().count() < width {
        line.to_string()
    } else {
        let cut: String = line.chars().take(width - 2).collect();
        format!("{cut}…")
    }
}

/// Everything a sync shows: event lines, the live status line on a terminal,
/// streamed JSON, and the log file.
struct Ui {
    json: bool,
    quiet: bool,
    tty: bool,
    dry_run: bool,
    run_id: Option<i64>,
    started: Instant,
    phase: String,
    total: usize,
    done: usize,
    counts: BTreeMap<String, usize>,
    current: Vec<(String, Instant)>,
    /// Lines describing what went wrong, for the summary and `on_problem`.
    trouble: Vec<String>,
    drawn: bool,
    daemon: bool,
    unlogged_start: bool,
    last_plain: Instant,
    log: Option<File>,
}

impl Ui {
    fn new(o: &Opts, run_id: Option<i64>) -> Ui {
        Ui {
            json: o.json,
            quiet: o.quiet,
            tty: !o.json && !o.quiet && std::io::stderr().is_terminal(),
            dry_run: o.dry_run,
            run_id,
            started: Instant::now(),
            phase: "starting".to_string(),
            total: 0,
            done: 0,
            counts: BTreeMap::new(),
            current: Vec::new(),
            trouble: Vec::new(),
            drawn: false,
            daemon: o.daemon,
            unlogged_start: false,
            last_plain: Instant::now(),
            log: if o.dry_run { None } else { open_log() },
        }
    }

    fn count(&self, action: &str) -> usize {
        self.counts.get(action).copied().unwrap_or(0)
    }

    fn log(&mut self, text: &str) {
        if let (Some(file), Some(id)) = (&mut self.log, self.run_id) {
            if std::mem::take(&mut self.unlogged_start) {
                let _ = writeln!(file, "{}  run {id}  started", util::now());
            }
            let _ = writeln!(file, "{}  run {id}  {text}", util::now());
        }
    }

    fn begin(&mut self) {
        if self.json {
            println!(
                "{}",
                json!({ "event": "start", "run_id": self.run_id, "dry_run": self.dry_run })
            );
        }
        if self.daemon {
            // Written only if the cycle turns out to have something to say.
            self.unlogged_start = true;
        } else {
            self.log("started");
        }
    }

    fn phase(&mut self, text: String) {
        self.phase = text;
        self.draw();
    }

    fn planned(&mut self, jobs: usize, unchanged: usize) {
        self.total = jobs;
        if unchanged > 0 {
            self.counts.insert("unchanged".to_string(), unchanged);
        }
        self.phase = "syncing".to_string();
        if self.json {
            println!(
                "{}",
                json!({ "event": "plan", "jobs": jobs, "unchanged": unchanged })
            );
        }
        self.draw();
    }

    fn clear(&mut self) {
        if self.drawn {
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
            self.drawn = false;
        }
    }

    fn draw(&mut self) {
        if !self.tty {
            return;
        }
        let now = Instant::now();
        let current: Vec<(String, u64)> = self
            .current
            .iter()
            .map(|(label, since)| (label.clone(), (now - *since).as_secs()))
            .collect();
        let line = status_line(
            &self.phase,
            self.done,
            self.total,
            &self.counts,
            self.started.elapsed().as_secs(),
            &current,
        );
        eprint!("\r\x1b[2K{}", fit(&line));
        let _ = std::io::stderr().flush();
        self.drawn = true;
    }

    /// Called when nothing happened for a moment: keeps the clock moving on a
    /// terminal, and shows signs of life in a log.
    fn tick(&mut self) {
        self.draw();
        let plain = !self.tty && !self.json && !self.quiet && self.total > 0;
        if plain && self.last_plain.elapsed() >= Duration::from_secs(30) {
            println!(
                "progress  {}/{} after {}",
                self.done,
                self.total,
                util::ago(self.started.elapsed().as_secs() as i64)
            );
            self.last_plain = Instant::now();
        }
    }

    fn event(&mut self, e: &Event) {
        *self.counts.entry(e.action.clone()).or_default() += 1;
        if self.json {
            let mut value = serde_json::to_value(e).unwrap_or_default();
            value["event"] = json!("repo");
            println!("{value}");
        }
        if e.action == "unchanged" {
            return;
        }
        let line = describe(e);
        let problem = e.action == "failed" || e.action == "skipped";
        if problem {
            self.trouble.push(line.clone());
        }
        self.log(&line);
        if self.json {
            return;
        }
        self.clear();
        if problem {
            eprintln!("{line}");
        } else if !self.quiet {
            println!("{line}");
        }
        self.draw();
    }

    fn finish(&mut self, problems: &[String], dry_run: bool) {
        self.clear();
        let seconds = self.started.elapsed().as_secs();
        let summary = summarize(&self.counts);
        for p in problems {
            self.log(&format!("problem   {p}"));
        }
        if !self.unlogged_start {
            self.log(&format!("finished: {summary} in {seconds}s"));
        }
        if self.json {
            println!(
                "{}",
                json!({
                    "event": "finish",
                    "run_id": self.run_id,
                    "dry_run": dry_run,
                    "counts": self.counts,
                    "problems": problems,
                    "seconds": seconds,
                    "ok": problems.is_empty() && self.trouble.is_empty(),
                })
            );
            return;
        }
        for p in problems {
            eprintln!("problem   {p}");
        }
        if !self.quiet {
            let prefix = if dry_run { "weir (dry run):" } else { "weir:" };
            println!("{prefix} {summary} in {seconds}s");
        }
    }
}

/// Opens `~/.weir/weir.log` for appending, starting a new file once it is big.
fn open_log() -> Option<File> {
    let path = db::home().join("weir.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5_000_000) {
        let _ = std::fs::rename(&path, db::home().join("weir.log.1"));
    }
    File::options().create(true).append(true).open(path).ok()
}

fn lock_file() -> Result<File> {
    let path = db::home().join("sync.lock");
    File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("could not open {}", path.display()))
}

/// Takes the lock that keeps two syncs from touching the same mirrors. With
/// `wait`, a sync that is already running is waited for instead of refused.
pub fn lock(wait: bool) -> Result<File> {
    let file = lock_file()?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) if wait => {
            eprintln!("weir: another sync is running; waiting for it to finish (see `weir watch`)");
            file.lock().context("could not take the sync lock")?;
            Ok(file)
        }
        Err(std::fs::TryLockError::WouldBlock) => bail!("another weir sync is already running"),
        Err(std::fs::TryLockError::Error(e)) => Err(e).context("could not take the sync lock"),
    }
}

/// Whether some process holds the sync lock right now.
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

/// Whether `endpoint` accepts a connection right now.
pub fn reachable(endpoint: &str) -> bool {
    wait_for(endpoint, 0, true)
}

/// Waits until `endpoint` accepts connections, for a machine that has just
/// woken up. Returns false when it stays unreachable for `secs` seconds.
fn wait_for(endpoint: &str, secs: u64, quiet: bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut announced = false;
    loop {
        let reachable = endpoint.to_socket_addrs().ok().is_some_and(|mut addrs| {
            addrs.any(|a| TcpStream::connect_timeout(&a, Duration::from_secs(5)).is_ok())
        });
        if reachable {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        if !announced && !quiet {
            eprintln!("waiting for {endpoint} to be reachable...");
            announced = true;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

fn valid_segment(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\', '\0'])
}

fn matches_any(patterns: &[String], name: &str) -> bool {
    let options = glob::MatchOptions {
        case_sensitive: false,
        ..Default::default()
    };
    patterns.iter().any(|p| {
        glob::Pattern::new(p)
            .map(|g| g.matches_with(name, options))
            .unwrap_or(false)
    })
}

/// Why a listed repository is or is not mirrored.
#[derive(PartialEq)]
enum Verdict {
    Wanted,
    /// Only the archived flag keeps it out.
    ArchivedOnly,
    Filtered,
}

fn verdict(t: &TrackedRow, r: &RemoteRepo) -> Verdict {
    let small_enough = match (t.max_size_mb, r.size_kb) {
        (Some(max), Some(kb)) => kb / 1024 <= max as u64,
        _ => true,
    };
    let by_name = small_enough
        && (t.forks || !r.fork)
        && (t.include.is_empty() || matches_any(&t.include, &r.name))
        && !matches_any(&t.exclude, &r.name);
    if !by_name {
        Verdict::Filtered
    } else if r.archived && !t.archived {
        Verdict::ArchivedOnly
    } else {
        Verdict::Wanted
    }
}

pub fn mirror_path(root: &Path, host: &str, owner: &str, name: &str) -> PathBuf {
    root.join(host).join(owner).join(name)
}

struct Credentials {
    token: String,
    login: String,
    name: String,
}

fn event(action: &str, repo: String, repo_id: i64, message: &str) -> Event {
    Event {
        action: action.to_string(),
        repo,
        repo_id: Some(repo_id),
        old_head: None,
        new_head: None,
        message: Some(message.to_string()),
        millis: None,
    }
}

fn plan(conn: &Connection, o: &Opts, root: &Path, ui: &mut Ui) -> Result<Plan> {
    let scope = target::resolve(conn, o.target.as_deref())?;
    let tracked: Vec<TrackedRow> = db::tracked(conn)?
        .into_iter()
        .filter(|t| scope.matches_tracked(t))
        .collect();
    if tracked.is_empty() {
        match &o.target {
            Some(t) => bail!("nothing tracked matches {t:?}; see `weir status`"),
            None => bail!("nothing is tracked yet; start with `weir track <host>/<owner>`"),
        }
    }
    let providers: HashMap<i64, ProviderRow> = db::providers(conn)?
        .into_iter()
        .map(|p| (p.id, p))
        .collect();
    let accounts: HashMap<i64, db::AccountRow> =
        db::accounts(conn)?.into_iter().map(|a| (a.id, a)).collect();

    let mut plan = Plan {
        jobs: Vec::new(),
        events: Vec::new(),
        problems: Vec::new(),
        unchanged: 0,
        rate_reset: None,
    };
    // What each provider last said about its rate limit.
    let mut limits: HashMap<i64, RateLimit> = HashMap::new();
    // One entry per account: its token, or None once it has failed. A failed
    // account is reported once and its other owners are skipped quietly.
    let mut creds: HashMap<i64, Option<Credentials>> = HashMap::new();
    let mut reachable: HashMap<i64, bool> = HashMap::new();
    let mut listed: Vec<(i64, Option<i64>)> = Vec::new();
    // Repositories that dropped out of a listing. What happened to them is
    // decided after every owner has been listed, because a repository that
    // moved to another tracked owner is not missing at all.
    let mut dropped: Vec<(RepoRow, i64, Option<i64>)> = Vec::new();

    for t in &tracked {
        let p = &providers[&t.provider_id];
        let label = if t.repo.is_empty() {
            format!("{}/{}", p.host, t.owner)
        } else {
            format!("{}/{}/{}", p.host, t.owner, t.repo)
        };
        ui.phase(format!("listing {label}"));
        let prov = match provider::build(p) {
            Ok(prov) => prov,
            Err(e) => {
                plan.problems.push(format!("{label}: {e:#}"));
                continue;
            }
        };

        let mut account_id = None;
        if prov.needs_account() {
            let Some(account) = t.account_id.and_then(|id| accounts.get(&id)) else {
                plan.problems.push(format!(
                    "{label}: no account; run `weir track {label} --account <name>`"
                ));
                continue;
            };
            account_id = Some(account.id);
            if let std::collections::hash_map::Entry::Vacant(slot) = creds.entry(account.id) {
                let resolved = Source::from_db(&account.source_kind, &account.source_value)
                    .and_then(|s| s.resolve(&account.name));
                let entry = match resolved {
                    Ok(token) => Some(Credentials {
                        token,
                        login: account.login.clone(),
                        name: account.name.clone(),
                    }),
                    Err(e) => {
                        plan.problems
                            .push(format!("account {}: {e:#}", account.name));
                        None
                    }
                };
                slot.insert(entry);
            }
            if creds[&account.id].is_none() {
                continue;
            }
        }
        let cred = account_id.and_then(|id| creds[&id].as_ref());
        // From here on, listings are conditional requests answered from the
        // cache when nothing changed. Each account has its own view.
        let prov = match provider::build_cached(
            p,
            Some(Cache {
                conn,
                scope: cred.map(|c| c.name.clone()).unwrap_or_default(),
            }),
        ) {
            Ok(prov) => prov,
            Err(e) => {
                plan.problems.push(format!("{label}: {e:#}"));
                continue;
            }
        };
        if let Some(limit) = limits.get(&p.id) {
            let wait = limit.reset - util::unix_now();
            if limit.remaining < RATE_FLOOR && wait > 0 {
                plan.problems.push(format!(
                    "{label}: not listed, the rate limit of {} is nearly used up; it resets in {}",
                    p.host,
                    util::ago(wait)
                ));
                continue;
            }
        }

        if let Some(endpoint) = prov.endpoint() {
            let up = *reachable
                .entry(p.id)
                .or_insert_with(|| wait_for(&endpoint, o.network_wait, o.quiet || o.json));
            if !up {
                plan.problems
                    .push(format!("{label}: could not reach {endpoint}"));
                continue;
            }
        }

        let token = cred.map(|c| c.token.as_str());
        let result = if t.repo.is_empty() {
            prov.list_repos(&t.owner, token, cred.map(|c| c.login.as_str()))
        } else {
            prov.get_repo(&t.owner, &t.repo, token)
                .map(|found| found.into_iter().collect())
        };
        if let Some(limit) = prov.rate_limit() {
            limits.insert(p.id, limit);
        }
        let remote = match result {
            Ok(r) => r,
            Err(e) => {
                match (e.downcast_ref::<AuthError>(), account_id, cred) {
                    (Some(auth), Some(id), Some(c)) => {
                        plan.problems.push(format!("account {}: {auth}", c.name));
                        creds.insert(id, None);
                    }
                    _ => plan.problems.push(format!("{label}: {e:#}")),
                }
                continue;
            }
        };

        // The listing succeeded, so it is safe to conclude what changed.
        let mut seen: HashSet<i64> = HashSet::new();
        let mut excluded: HashSet<String> = HashSet::new();
        let listed_ids: HashSet<&str> = remote.iter().map(|r| r.id.as_str()).collect();
        for r in &remote {
            if !valid_segment(&r.owner) || !valid_segment(&r.name) {
                plan.problems.push(format!(
                    "{label}: ignoring a repository with an unusable name ({:?}/{:?})",
                    r.owner, r.name
                ));
                continue;
            }
            let full = format!("{}/{}/{}", p.host, r.owner, r.name);
            let state = match verdict(t, r) {
                Verdict::Wanted => "active",
                Verdict::Filtered => {
                    excluded.insert(r.id.clone());
                    continue;
                }
                Verdict::ArchivedOnly => {
                    // An archived repository never changes again, so a mirror
                    // that already exists is kept and stays searchable. One
                    // that was never mirrored is not cloned.
                    let mirrored =
                        db::repos(conn, "provider_id = ?1 AND remote_id = ?2", &[&p.id, &r.id])?
                            .pop()
                            .is_some_and(|row| row.disk_name.is_some());
                    let on_disk = mirror_path(root, &p.host, &r.owner, &r.name)
                        .join(".git")
                        .is_dir();
                    if !mirrored && !on_disk {
                        excluded.insert(r.id.clone());
                        continue;
                    }
                    "archived"
                }
            };
            let up = upsert(conn, p.id, t.id, r, state, &listed_ids)?;
            seen.insert(up.id);
            if up.newly_archived {
                plan.events.push(event(
                    "archived",
                    full.clone(),
                    up.id,
                    "archived on the provider; mirror kept",
                ));
            }
            if up.recreated {
                plan.events.push(event(
                    "recreated",
                    full,
                    up.id,
                    "deleted and created again on the provider; the mirror follows the new one",
                ));
            }
        }
        for old in db::repos(
            conn,
            "tracked_id = ?1 AND state IN ('active', 'archived')",
            &[&t.id],
        )? {
            if seen.contains(&old.id) {
                continue;
            }
            if excluded.contains(&old.remote_id) {
                conn.execute(
                    "UPDATE repos SET state = 'excluded' WHERE id = ?1",
                    [old.id],
                )?;
                if scope.matches_repo(&old) {
                    plan.events.push(event(
                        "excluded",
                        format!("{}/{}/{}", p.host, old.owner, old.name),
                        old.id,
                        "now filtered out; mirror kept",
                    ));
                }
            } else {
                dropped.push((old, t.id, account_id));
            }
        }
        listed.push((t.id, account_id));
    }

    for (old, tracked_id, account_id) in dropped {
        // Picked up by another tracked owner in this run: it moved, it is not gone.
        let current = db::repos(conn, "id = ?1", &[&old.id])?.pop();
        if current.is_none_or(|row| row.tracked_id != Some(tracked_id)) {
            continue;
        }
        let p = &providers[&old.provider_id];
        let full = format!("{}/{}/{}", p.host, old.owner, old.name);
        let token = account_id
            .and_then(|id| creds.get(&id))
            .and_then(|c| c.as_ref())
            .map(|c| c.token.as_str());
        let found = provider::build(p).and_then(|prov| prov.lookup_id(&old.remote_id, token));
        let (state, message) = match found {
            Ok(Lookup::NotFound) => ("gone", "no longer on the provider; mirror kept".to_string()),
            Ok(Lookup::Forbidden) => (
                "inaccessible",
                "still exists, but access is refused; mirror kept".to_string(),
            ),
            Ok(Lookup::Found(now)) if !now.owner.eq_ignore_ascii_case(&old.owner) => (
                "transferred",
                format!(
                    "now {}/{}, which is not tracked; mirror kept",
                    now.owner, now.name
                ),
            ),
            Ok(Lookup::Found(_)) => (
                "inaccessible",
                "still exists, but is no longer listed for this account; mirror kept".to_string(),
            ),
            Err(e) => {
                // Not knowing is not a reason to change anything.
                plan.problems.push(format!(
                    "{full}: left the listing and could not be checked: {e:#}"
                ));
                continue;
            }
        };
        conn.execute(
            "UPDATE repos SET state = ?1 WHERE id = ?2",
            params![state, old.id],
        )?;
        if scope.matches_repo(&old) {
            plan.events.push(event(state, full, old.id, &message));
        }
    }

    // Unmanaged clones are only looked at when something is about to be
    // cloned, to see whether one of them is that repository under an old name.
    let mut strays: Option<Vec<stray::Stray>> = None;
    let mut stray_ids: HashMap<PathBuf, Option<String>> = HashMap::new();

    for (tracked_id, account_id) in &listed {
        let cred = account_id
            .and_then(|id| creds.get(&id))
            .and_then(|c| c.as_ref());
        let token = cred.map(|c| c.token.as_str());
        for r in db::repos(
            conn,
            "tracked_id = ?1 AND state IN ('active', 'archived')",
            &[tracked_id],
        )? {
            if !scope.matches_repo(&r) {
                continue;
            }
            let p = &providers[&r.provider_id];
            let host = &p.host;
            let path = mirror_path(root, host, &r.owner, &r.name);
            let synced_before = r.disk_name.is_some();
            let mut move_from = match (&r.disk_owner, &r.disk_name) {
                (Some(o), Some(n))
                    if !(o.eq_ignore_ascii_case(&r.owner) && n.eq_ignore_ascii_case(&r.name)) =>
                {
                    Some(mirror_path(root, host, o, n)).filter(|from| from.exists())
                }
                _ => None,
            };
            let mut accept_origin = false;
            if !synced_before && path.join(".git").is_dir() {
                // A clone that predates weir may still point at an old name of
                // this repository. The provider knows whether it is the same one.
                accept_origin = old_name_of(p, &path, &r, token).unwrap_or(false);
            } else if !synced_before && !path.exists() {
                if strays.is_none() {
                    strays = Some(stray::scan(conn, root)?);
                }
                for s in strays.iter().flatten().filter(|s| {
                    s.provider_id == r.provider_id && s.owner.eq_ignore_ascii_case(&r.owner)
                }) {
                    let id = stray_ids.entry(s.path.clone()).or_insert_with(|| {
                        let origin =
                            git::opt(Some(&s.path), &["config", "--get", "remote.origin.url"])?;
                        let (owner, name) = stray::origin_owner_name(p, &origin)?;
                        match provider::build(p).ok()?.lookup(&owner, &name, token) {
                            Ok(Lookup::Found(found)) => Some(found.id),
                            _ => None,
                        }
                    });
                    if id.as_deref() == Some(r.remote_id.as_str()) {
                        move_from = Some(s.path.clone());
                        break;
                    }
                }
            }
            let settled = r.last_result.as_deref() == Some("ok") && synced_before;
            let fetch = o.all
                || !settled
                || !path.exists()
                || r.pushed_at.is_none()
                || r.pushed_at != r.synced_pushed_at
                || r.synced_branch.as_deref() != Some(r.default_branch.as_str());
            if !fetch && move_from.is_none() && head_in_place(&path, r.head.as_deref()) {
                plan.unchanged += 1;
                continue;
            }
            plan.jobs.push(Job {
                repo_id: r.id,
                repo: format!("{host}/{}/{}", r.owner, r.name),
                path,
                clone_url: r.clone_url.clone(),
                default_branch: r.default_branch.clone(),
                account: cred.map(|c| c.name.clone()),
                fetch,
                known: synced_before,
                move_from,
                accept_origin,
                size_kb: r.size_kb,
            });
        }
    }
    plan.rate_reset = limits
        .values()
        .filter(|l| l.remaining < RATE_FLOOR)
        .map(|l| l.reset)
        .max();
    Ok(plan)
}

/// Whether the clone at `path` points at an old name of repository `r`.
fn old_name_of(p: &ProviderRow, path: &Path, r: &RepoRow, token: Option<&str>) -> Option<bool> {
    let origin = git::opt(Some(path), &["config", "--get", "remote.origin.url"])?;
    if git::normalize_url(&origin) == git::normalize_url(&r.clone_url) {
        return Some(false);
    }
    let (owner, name) = stray::origin_owner_name(p, &origin)?;
    match provider::build(p).ok()?.lookup(&owner, &name, token) {
        Ok(Lookup::Found(found)) => Some(found.id == r.remote_id),
        _ => Some(false),
    }
}

/// A cheap check that a mirror's HEAD is still detached where weir left it,
/// done by reading one file, so it can run on every mirror on every sync.
fn head_in_place(path: &Path, expected: Option<&str>) -> bool {
    let git_dir = path.join(".git");
    // A leftover marker means a checkout was interrupted and must be finished.
    if git_dir.join(git::CHECKOUT_MARKER).exists() {
        return false;
    }
    let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD")) else {
        return false;
    };
    match expected {
        Some(sha) => head.trim() == sha,
        // An empty repository: HEAD points at a branch that has no commits.
        None => head.starts_with("ref:"),
    }
}

struct Upserted {
    id: i64,
    /// It was active before and the provider now reports it archived.
    newly_archived: bool,
    /// A new repository took over the name, and the row, of one that is gone.
    recreated: bool,
}

/// Inserts or updates the row for a listed repository.
fn upsert(
    conn: &Connection,
    provider_id: i64,
    tracked_id: i64,
    r: &RemoteRepo,
    state: &str,
    listed: &HashSet<&str>,
) -> Result<Upserted> {
    let before: Option<String> = conn
        .query_row(
            "SELECT state FROM repos WHERE provider_id = ?1 AND remote_id = ?2",
            params![provider_id, r.id],
            |row| row.get(0),
        )
        .optional()?;
    // A repository deleted and recreated under the same name gets a new id on
    // the provider. Drop the row of the one it replaced, so one mirror never
    // has two owners. A row whose repository is still in this listing is left
    // alone: that is two repositories trading names, not a replacement.
    let mut replaced = 0;
    for other in db::repos(
        conn,
        "provider_id = ?1 AND owner = ?2 AND name = ?3 AND remote_id <> ?4",
        &[&provider_id, &r.owner, &r.name, &r.id],
    )? {
        if !listed.contains(other.remote_id.as_str()) {
            replaced += conn.execute("DELETE FROM repos WHERE id = ?1", [other.id])?;
        }
    }
    conn.execute(
        "INSERT INTO repos (provider_id, tracked_id, remote_id, owner, name, default_branch,
                            clone_url, pushed_at, fork, archived, state, size_kb)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT (provider_id, remote_id) DO UPDATE SET
           tracked_id = excluded.tracked_id, owner = excluded.owner, name = excluded.name,
           default_branch = excluded.default_branch, clone_url = excluded.clone_url,
           pushed_at = excluded.pushed_at, fork = excluded.fork, archived = excluded.archived,
           state = excluded.state, size_kb = excluded.size_kb",
        params![
            provider_id,
            tracked_id,
            r.id,
            r.owner,
            r.name,
            r.default_branch,
            r.clone_url,
            r.pushed_at,
            r.fork,
            r.archived,
            state,
            r.size_kb.map(|kb| kb as i64)
        ],
    )?;
    let id = conn.query_row(
        "SELECT id FROM repos WHERE provider_id = ?1 AND remote_id = ?2",
        params![provider_id, r.id],
        |row| row.get(0),
    )?;
    Ok(Upserted {
        id,
        newly_archived: before.as_deref() == Some("active") && state == "archived",
        recreated: replaced > 0 && before.is_none(),
    })
}

/// Used by `status` and `list`: the mirror's location for a repository row.
pub fn path_of(root: &Path, host: &str, r: &RepoRow) -> PathBuf {
    mirror_path(root, host, &r.owner, &r.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_shows_progress_and_what_is_in_flight() {
        let mut counts = BTreeMap::new();
        assert_eq!(
            status_line("listing github.com/acme", 0, 0, &counts, 12, &[]),
            "listing github.com/acme · 12s"
        );
        assert_eq!(
            status_line(
                "syncing",
                0,
                1,
                &counts,
                9,
                &[("github.com/acme/big (1.0 GB)".to_string(), 9)]
            ),
            "[0/1] · 9s · big (1.0 GB)"
        );
        counts.insert("cloned".to_string(), 3);
        counts.insert("unchanged".to_string(), 380);
        let current = vec![
            ("github.com/acme/big (14.1 GB)".to_string(), 125),
            ("github.com/acme/web".to_string(), 2),
            ("github.com/acme/api".to_string(), 1),
            ("github.com/acme/cli".to_string(), 0),
        ];
        assert_eq!(
            status_line("syncing", 412, 709, &counts, 190, &current),
            "[412/709] 3 cloned, 380 unchanged · 3m10s · big (14.1 GB) 2m05s, web, api, +1"
        );
    }
}
