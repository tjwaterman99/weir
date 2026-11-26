//! The sync engine: ask each provider what exists, compare with the database,
//! then clone, fetch, move or repair mirrors until the disk matches.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use crate::db::{self, ProviderRow, RepoRow, TrackedRow};
use crate::git::{self, Action, Job};
use crate::provider::{self, AuthError, RemoteRepo};
use crate::secret::Source;
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
}

struct Plan {
    jobs: Vec<Job>,
    /// Changes found while listing: repositories that are gone or filtered out.
    events: Vec<Event>,
    problems: Vec<String>,
    unchanged: usize,
}

#[derive(Serialize)]
struct Report<'a> {
    run_id: Option<i64>,
    dry_run: bool,
    counts: &'a BTreeMap<String, usize>,
    events: &'a [Event],
    problems: &'a [String],
}

/// Runs a sync and returns the process exit code.
pub fn run(conn: &Connection, o: &Opts) -> Result<i32> {
    let root = db::root(conn)?;
    let _lock = if o.dry_run { None } else { Some(lock()?) };
    let started = Instant::now();

    // Planning writes what the providers report into the database. A dry run
    // rolls all of it back, so it leaves no trace.
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let plan = match plan(conn, o, &root) {
        Ok(p) => p,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    };
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut events = plan.events.clone();

    if o.dry_run {
        conn.execute_batch("ROLLBACK")?;
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
            events.push(Event {
                action: action.to_string(),
                repo: job.repo.clone(),
                repo_id: Some(job.repo_id),
                old_head: None,
                new_head: None,
                message: None,
            });
        }
        for e in &events {
            *counts.entry(e.action.clone()).or_default() += 1;
            emit(o, e);
        }
        counts.insert("unchanged".to_string(), plan.unchanged);
        finish(o, None, true, &counts, &events, &plan.problems, started);
        return Ok(if plan.problems.is_empty() { 0 } else { 1 });
    }
    conn.execute_batch("COMMIT")?;

    conn.execute(
        "INSERT INTO sync_runs (started_at) VALUES (?1)",
        [util::now()],
    )?;
    let run_id = conn.last_insert_rowid();
    for e in &events {
        record_event(conn, run_id, e)?;
        *counts.entry(e.action.clone()).or_default() += 1;
        emit(o, e);
    }

    let pushed: HashMap<i64, (Option<String>, String, String, String)> =
        db::repos(conn, "state = 'active'", &[])?
            .into_iter()
            .map(|r| (r.id, (r.pushed_at, r.default_branch, r.owner, r.name)))
            .collect();

    let queue = Mutex::new(plan.jobs.into_iter());
    let (tx, rx) = mpsc::channel();
    let workers = o.jobs.max(1);
    let mut failure: Option<anyhow::Error> = None;
    std::thread::scope(|s| {
        for _ in 0..workers {
            let tx = tx.clone();
            let queue = &queue;
            s.spawn(move || loop {
                let job = queue.lock().unwrap().next();
                let Some(job) = job else { break };
                let outcome = git::sync_mirror(&job);
                if tx.send((job, outcome)).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        for (job, outcome) in rx {
            let event = Event {
                action: outcome.action.as_str().to_string(),
                repo: job.repo.clone(),
                repo_id: Some(job.repo_id),
                old_head: outcome.old_head.clone(),
                new_head: outcome.new_head.clone(),
                message: outcome.message(),
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
                if outcome.action != Action::Unchanged {
                    record_event(conn, run_id, &event)?;
                }
                Ok(())
            })();
            if let Err(e) = stored {
                failure.get_or_insert(e);
            }
            *counts.entry(event.action.clone()).or_default() += 1;
            if outcome.action != Action::Unchanged {
                emit(o, &event);
                events.push(event);
            }
        }
    });
    if let Some(e) = failure {
        return Err(e.context("could not record sync results"));
    }

    *counts.entry("unchanged".to_string()).or_default() += plan.unchanged;
    let mut summary = serde_json::to_value(&counts)?;
    summary["problems"] = serde_json::json!(plan.problems);
    conn.execute(
        "UPDATE sync_runs SET finished_at = ?1, seconds = ?2, summary = ?3 WHERE id = ?4",
        params![
            util::now(),
            started.elapsed().as_secs() as i64,
            summary.to_string(),
            run_id
        ],
    )?;
    finish(
        o,
        Some(run_id),
        false,
        &counts,
        &events,
        &plan.problems,
        started,
    );

    let bad =
        counts.get("failed").copied().unwrap_or(0) + counts.get("skipped").copied().unwrap_or(0);
    Ok(if bad > 0 || !plan.problems.is_empty() {
        1
    } else {
        0
    })
}

fn record_event(conn: &Connection, run_id: i64, e: &Event) -> Result<()> {
    conn.execute(
        "INSERT INTO repo_syncs (run_id, repo_id, repo, action, old_head, new_head, message)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![run_id, e.repo_id, e.repo, e.action, e.old_head, e.new_head, e.message],
    )?;
    Ok(())
}

/// Formats an event the way `sync` and `log` both print it.
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

fn emit(o: &Opts, e: &Event) {
    if o.json {
        return;
    }
    let problem = e.action == "failed" || e.action == "skipped";
    if problem {
        eprintln!("{}", describe(e));
    } else if !o.quiet {
        println!("{}", describe(e));
    }
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

fn finish(
    o: &Opts,
    run_id: Option<i64>,
    dry_run: bool,
    counts: &BTreeMap<String, usize>,
    events: &[Event],
    problems: &[String],
    started: Instant,
) {
    if o.json {
        let report = Report {
            run_id,
            dry_run,
            counts,
            events,
            problems,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
        return;
    }
    for p in problems {
        eprintln!("problem   {p}");
    }
    if !o.quiet {
        let prefix = if dry_run { "weir (dry run):" } else { "weir:" };
        println!(
            "{prefix} {} in {}s",
            summarize(counts),
            started.elapsed().as_secs()
        );
    }
}

fn lock() -> Result<File> {
    let path = db::home().join("sync.lock");
    let file = File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("could not open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => bail!("another weir sync is already running"),
        Err(std::fs::TryLockError::Error(e)) => Err(e).context("could not take the sync lock"),
    }
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

fn wanted(t: &TrackedRow, r: &RemoteRepo) -> bool {
    (t.forks || !r.fork)
        && (t.archived || !r.archived)
        && (t.include.is_empty() || matches_any(&t.include, &r.name))
        && !matches_any(&t.exclude, &r.name)
}

pub fn mirror_path(root: &Path, host: &str, owner: &str, name: &str) -> PathBuf {
    root.join(host).join(owner).join(name)
}

struct Credentials {
    token: String,
    login: String,
    name: String,
}

fn plan(conn: &Connection, o: &Opts, root: &Path) -> Result<Plan> {
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
    };
    // One entry per account: its token, or None once it has failed. A failed
    // account is reported once and its other owners are skipped quietly.
    let mut creds: HashMap<i64, Option<Credentials>> = HashMap::new();
    let mut reachable: HashMap<i64, bool> = HashMap::new();
    let mut listed: Vec<(i64, Option<String>)> = Vec::new();

    for t in &tracked {
        let p = &providers[&t.provider_id];
        let label = if t.repo.is_empty() {
            format!("{}/{}", p.host, t.owner)
        } else {
            format!("{}/{}/{}", p.host, t.owner, t.repo)
        };
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

        // The listing succeeded, so it is safe to conclude what is gone.
        let mut seen: HashSet<i64> = HashSet::new();
        let mut excluded: HashSet<String> = HashSet::new();
        for r in &remote {
            if !valid_segment(&r.owner) || !valid_segment(&r.name) {
                plan.problems.push(format!(
                    "{label}: ignoring a repository with an unusable name ({:?}/{:?})",
                    r.owner, r.name
                ));
                continue;
            }
            if !wanted(t, r) {
                excluded.insert(r.id.clone());
                continue;
            }
            seen.insert(upsert(conn, p.id, t.id, r)?);
        }
        for old in db::repos(conn, "tracked_id = ?1 AND state = 'active'", &[&t.id])? {
            if seen.contains(&old.id) {
                continue;
            }
            let state = if excluded.contains(&old.remote_id) {
                "excluded"
            } else {
                "gone"
            };
            conn.execute(
                "UPDATE repos SET state = ?1 WHERE id = ?2",
                params![state, old.id],
            )?;
            if scope.matches_repo(&old) {
                plan.events.push(Event {
                    action: state.to_string(),
                    repo: format!("{}/{}/{}", p.host, old.owner, old.name),
                    repo_id: Some(old.id),
                    old_head: None,
                    new_head: None,
                    message: Some(
                        if state == "gone" {
                            "no longer on the provider; mirror kept"
                        } else {
                            "now filtered out; mirror kept"
                        }
                        .to_string(),
                    ),
                });
            }
        }
        listed.push((t.id, cred.map(|c| c.name.clone())));
    }

    for (tracked_id, account) in &listed {
        for r in db::repos(conn, "tracked_id = ?1 AND state = 'active'", &[tracked_id])? {
            if !scope.matches_repo(&r) {
                continue;
            }
            let host = &providers[&r.provider_id].host;
            let path = mirror_path(root, host, &r.owner, &r.name);
            let move_from = match (&r.disk_owner, &r.disk_name) {
                (Some(o), Some(n))
                    if !(o.eq_ignore_ascii_case(&r.owner) && n.eq_ignore_ascii_case(&r.name)) =>
                {
                    Some(mirror_path(root, host, o, n)).filter(|from| from.exists())
                }
                _ => None,
            };
            let known = r.last_result.as_deref() == Some("ok") && r.disk_name.is_some();
            let fetch = o.all
                || !known
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
                account: account.clone(),
                fetch,
                known: r.disk_name.is_some(),
                move_from,
            });
        }
    }
    Ok(plan)
}

/// A cheap check that a mirror's HEAD is still detached where weir left it,
/// done by reading one file, so it can run on every mirror on every sync.
fn head_in_place(path: &Path, expected: Option<&str>) -> bool {
    let Ok(head) = std::fs::read_to_string(path.join(".git").join("HEAD")) else {
        return false;
    };
    match expected {
        Some(sha) => head.trim() == sha,
        // An empty repository: HEAD points at a branch that has no commits.
        None => head.starts_with("ref:"),
    }
}

/// Inserts or updates the row for a listed repository and returns its id.
fn upsert(conn: &Connection, provider_id: i64, tracked_id: i64, r: &RemoteRepo) -> Result<i64> {
    // A repository deleted and recreated under the same name gets a new id on
    // the provider. Drop the old row so one mirror never has two owners.
    conn.execute(
        "DELETE FROM repos
         WHERE provider_id = ?1 AND owner = ?2 AND name = ?3 AND remote_id <> ?4
           AND state <> 'active'",
        params![provider_id, r.owner, r.name, r.id],
    )?;
    conn.execute(
        "INSERT INTO repos (provider_id, tracked_id, remote_id, owner, name, default_branch,
                            clone_url, pushed_at, fork, archived, state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'active')
         ON CONFLICT (provider_id, remote_id) DO UPDATE SET
           tracked_id = excluded.tracked_id, owner = excluded.owner, name = excluded.name,
           default_branch = excluded.default_branch, clone_url = excluded.clone_url,
           pushed_at = excluded.pushed_at, fork = excluded.fork, archived = excluded.archived,
           state = 'active'",
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
            r.archived
        ],
    )?;
    Ok(conn.query_row(
        "SELECT id FROM repos WHERE provider_id = ?1 AND remote_id = ?2",
        params![provider_id, r.id],
        |row| row.get(0),
    )?)
}

/// Used by `status` and `list`: the mirror's location for a repository row.
pub fn path_of(root: &Path, host: &str, r: &RepoRow) -> PathBuf {
    mirror_path(root, host, &r.owner, &r.name)
}
