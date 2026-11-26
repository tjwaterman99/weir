mod daemon;
mod db;
mod git;
mod provider;
mod reference;
mod runs;
mod secret;
mod service;
mod settings;
mod stray;
mod sync;
mod target;
mod util;
mod watch;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;
use std::io::{BufRead, IsTerminal, Read, Write};

use secret::Source;

#[derive(Parser)]
#[command(
    name = "weir",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("WEIR_COMMIT"), ")"),
    about = "Keeps a set of repositories from a provider mirrored locally, read-only",
    disable_help_subcommand = true
)]
struct Cli {
    /// Machine-readable output
    #[arg(long, global = true)]
    json: bool,
    /// Print errors only
    #[arg(long, short, global = true)]
    quiet: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage the accounts weir signs in to providers with
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Start mirroring an owner (an org or a user) or a single repository
    Track {
        /// <host>/<owner> or <host>/<owner>/<repo>
        target: String,
        /// The account to use; required once the host has more than one
        #[arg(long)]
        account: Option<String>,
        /// Only repositories whose name matches this glob (repeatable)
        #[arg(long)]
        include: Vec<String>,
        /// Skip repositories whose name matches this glob (repeatable)
        #[arg(long)]
        exclude: Vec<String>,
        /// Also mirror forks
        #[arg(long)]
        forks: bool,
        /// Also mirror archived repositories
        #[arg(long)]
        archived: bool,
        /// Skip repositories larger than this many megabytes
        #[arg(long, value_name = "MB")]
        max_size: Option<i64>,
    },
    /// Stop mirroring an owner or repository; its mirrors stay on disk
    Untrack {
        /// <host>/<owner> or <host>/<owner>/<repo>
        target: String,
    },
    /// Bring every mirror in line with its provider
    Sync {
        /// Limit the sync to a host, an owner or one repository
        target: Option<String>,
        /// Show what would be cloned, fetched or moved, and change nothing
        #[arg(long)]
        dry_run: bool,
        /// Fetch every repository, even those with no new pushes
        #[arg(long)]
        all: bool,
        /// How many repositories to work on at once
        #[arg(long, default_value_t = 8)]
        jobs: usize,
        /// Seconds to wait for the provider to become reachable
        #[arg(long, default_value_t = 60)]
        network_wait: u64,
        /// Fail at once if another sync is running, instead of waiting for it
        #[arg(long)]
        no_wait: bool,
    },
    /// Delete mirrors that are no longer tracked or are gone from the provider
    Prune {
        /// List what would be deleted, and delete nothing
        #[arg(long)]
        dry_run: bool,
        /// Do not ask for confirmation
        #[arg(long, short)]
        yes: bool,
        /// Also delete mirrors of repositories that were archived upstream
        #[arg(long)]
        archived: bool,
        /// Also delete stale copies that weir does not manage (see `weir doctor`)
        #[arg(long)]
        unmanaged: bool,
    },
    /// Explain the git repositories under the root that weir does not manage
    Doctor,
    /// Show accounts, what is tracked, the last sync and mirrors needing attention
    Status { target: Option<String> },
    /// List mirrored repositories, one per line
    List {
        target: Option<String>,
        /// Include mirrors of repositories that were archived upstream
        #[arg(long)]
        archived: bool,
        /// Include every repository weir knows, whatever its state
        #[arg(long)]
        all: bool,
    },
    /// Print the path of a repository's mirror
    Path {
        /// <repo>, <owner>/<repo> or <host>/<owner>/<repo>
        repo: String,
    },
    /// Show past sync runs and what each one changed
    Log {
        /// How many runs to show
        #[arg(short = 'n', default_value_t = 5)]
        count: usize,
        /// Also list repositories that were fetched and had not changed
        #[arg(long)]
        all: bool,
        /// Show the ten slowest repositories of each run instead
        #[arg(long, conflicts_with = "all")]
        slow: bool,
    },
    /// Follow a sync that is running, from any terminal
    Watch {
        /// Keep watching after it ends, and follow the next ones too
        #[arg(long)]
        follow: bool,
    },
    /// Keep every mirror current: sync, wait, repeat, until stopped
    Daemon {
        /// Seconds between cycles (default: the `interval_seconds` setting)
        #[arg(long)]
        interval: Option<u64>,
        /// Stop after this many cycles
        #[arg(long, hide = true)]
        cycles: Option<u64>,
    },
    /// Run the daemon as a service that starts at login
    #[command(subcommand)]
    Service(ServiceCmd),
    /// Manage provider hosts (only needed for self-hosted providers)
    #[command(subcommand)]
    Provider(ProviderCmd),
    /// Read or change settings
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Print the version
    Version,
    /// Write the documentation site's command reference
    #[command(hide = true)]
    Reference {
        /// Directory to write the pages into
        dir: std::path::PathBuf,
        /// Directory of hand-written example files, one per command
        #[arg(long)]
        examples: std::path::PathBuf,
    },
    /// The git credential helper weir installs in each mirror
    #[command(hide = true)]
    Credential {
        #[arg(long)]
        account: String,
        operation: Option<String>,
    },
}

#[derive(Subcommand)]
enum AuthCmd {
    /// Add an account, or replace the token of an existing one
    Login {
        /// The provider host, for example github.com
        host: String,
        /// A name for the account; defaults to its login on the provider
        #[arg(long)]
        name: Option<String>,
        /// Read the token from this environment variable each time
        #[arg(long, conflicts_with = "command")]
        env: Option<String>,
        /// Run this command to get the token each time, e.g. "gh auth token"
        #[arg(long)]
        command: Option<String>,
    },
    /// List accounts and check that each token still works
    List,
    /// Remove an account
    Logout { name: String },
}

#[derive(Subcommand)]
enum ProviderCmd {
    /// Add a provider host
    Add {
        /// github, gitlab, bitbucket or local
        kind: String,
        host: String,
        /// The API base URL; for `local`, a directory of bare repositories
        #[arg(long)]
        api_url: Option<String>,
    },
    /// List provider hosts
    List,
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// Register the daemon with launchd (macOS) or systemd (Linux) and start it
    Install,
    /// Stop the daemon and remove the service
    Uninstall,
    /// Start the daemon now
    Start,
    /// Stop the daemon until the next login
    Stop,
    /// Say whether the service is installed and the daemon is running
    Status,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Show every setting, its value and what it does
    List,
    /// Print a setting
    Get { key: String },
    /// Change a setting
    Set { key: String, value: String },
    /// Put a setting back to its default
    Unset { key: String },
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("weir: {e:#}");
            2
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> Result<i32> {
    if let Cmd::Version = cli.cmd {
        println!(
            "weir {} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("WEIR_COMMIT")
        );
        return Ok(0);
    }
    if let Cmd::Reference { dir, examples } = &cli.cmd {
        use clap::CommandFactory;
        reference::write(Cli::command(), dir, examples)?;
        return Ok(0);
    }
    let conn = db::open()?;
    match cli.cmd {
        Cmd::Auth(AuthCmd::Login {
            host,
            name,
            env,
            command,
        }) => auth_login(&conn, &host, name, env, command)?,
        Cmd::Auth(AuthCmd::List) => return auth_list(&conn, cli.json),
        Cmd::Auth(AuthCmd::Logout { name }) => auth_logout(&conn, &name)?,
        Cmd::Track {
            target,
            account,
            include,
            exclude,
            forks,
            archived,
            max_size,
        } => track(
            &conn, &target, account, include, exclude, forks, archived, max_size,
        )?,
        Cmd::Untrack { target } => untrack(&conn, &target)?,
        Cmd::Sync {
            target,
            dry_run,
            all,
            jobs,
            network_wait,
            no_wait,
        } => {
            return sync::run(
                &conn,
                &sync::Opts {
                    target,
                    dry_run,
                    all,
                    jobs,
                    network_wait,
                    json: cli.json,
                    quiet: cli.quiet,
                    wait: !no_wait,
                    daemon: false,
                    stop: None,
                },
            )
        }
        Cmd::Prune {
            dry_run,
            yes,
            archived,
            unmanaged,
        } => prune(&conn, dry_run, yes, archived, unmanaged)?,
        Cmd::Doctor => doctor(&conn, cli.json)?,
        Cmd::Status { target } => return status(&conn, target.as_deref(), cli.json),
        Cmd::List {
            target,
            archived,
            all,
        } => list(&conn, target.as_deref(), archived, all, cli.json)?,
        Cmd::Path { repo } => path(&conn, &repo)?,
        Cmd::Log { count, all, slow } => log(&conn, count, all, slow, cli.json)?,
        Cmd::Watch { follow } => return watch::watch(&conn, follow),
        Cmd::Provider(ProviderCmd::Add {
            kind,
            host,
            api_url,
        }) => provider_add(&conn, &kind, &host, api_url)?,
        Cmd::Provider(ProviderCmd::List) => {
            for p in db::providers(&conn)? {
                println!("{:<10} {:<24} {}", p.kind, p.host, p.api_url);
            }
        }
        Cmd::Daemon { interval, cycles } => {
            return daemon::run(&conn, &daemon::Opts { interval, cycles })
        }
        Cmd::Service(ServiceCmd::Install) => service::install()?,
        Cmd::Service(ServiceCmd::Uninstall) => service::uninstall()?,
        Cmd::Service(ServiceCmd::Start) => service::start()?,
        Cmd::Service(ServiceCmd::Stop) => service::stop()?,
        Cmd::Service(ServiceCmd::Status) => return service::status(&conn),
        Cmd::Config(ConfigCmd::List) => config_list(&conn, cli.json)?,
        Cmd::Config(ConfigCmd::Get { key }) => config_get(&conn, &key)?,
        Cmd::Config(ConfigCmd::Set { key, value }) => config_set(&conn, &key, &value)?,
        Cmd::Config(ConfigCmd::Unset { key }) => {
            settings::unset(&conn, &key)?;
            println!("{key} is back to its default.");
        }
        Cmd::Credential { account, operation } => credential(&conn, &account, operation)?,
        Cmd::Version | Cmd::Reference { .. } => unreachable!(),
    }
    Ok(0)
}

// ------------------------------------------------------------------ auth

fn read_token(host: &str) -> Result<String> {
    let token = if std::io::stdin().is_terminal() {
        rpassword::prompt_password(format!("Token for {host}: "))?
    } else {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        line
    };
    let token = token.trim().to_string();
    if token.is_empty() {
        bail!("no token was given");
    }
    Ok(token)
}

fn valid_account_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn auth_login(
    conn: &Connection,
    host: &str,
    name: Option<String>,
    env: Option<String>,
    command: Option<String>,
) -> Result<()> {
    let Some(p) = db::provider_by_host(conn, host)? else {
        bail!("{host:?} is not a known provider host; add it with `weir provider add`");
    };
    let prov = provider::build(&p)?;
    if !prov.needs_account() {
        bail!("{host} needs no account");
    }
    let source = match (env, command) {
        (Some(var), _) => Source::Env(var),
        (_, Some(cmd)) => Source::Command(cmd),
        _ => Source::Keychain,
    };
    let token = match &source {
        Source::Keychain => read_token(host)?,
        other => other.resolve("")?,
    };
    let who = prov
        .whoami(&token)
        .with_context(|| format!("could not sign in to {host}"))?;
    let name = name.unwrap_or_else(|| who.login.clone());
    if !valid_account_name(&name) {
        bail!("account names may only use letters, digits, '-', '_' and '.'; pass --name");
    }
    if let Some(existing) = db::account_by_name(conn, &name)? {
        if existing.provider_id != p.id {
            bail!("the account name {name:?} is already used for another host");
        }
    }
    if source == Source::Keychain {
        secret::keychain_set(&name, &token).context("could not store the token in the keychain")?;
    }
    conn.execute(
        "INSERT INTO accounts (provider_id, name, login, remote_user_id, source_kind, source_value, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (name) DO UPDATE SET
           login = excluded.login, remote_user_id = excluded.remote_user_id,
           source_kind = excluded.source_kind, source_value = excluded.source_value",
        params![p.id, name, who.login, who.id, source.kind(), source.value(), util::now()],
    )?;
    println!(
        "Signed in to {} as {} (account {name:?}, token from {}).",
        p.host,
        who.login,
        source.describe()
    );
    Ok(())
}

fn auth_list(conn: &Connection, as_json: bool) -> Result<i32> {
    let providers = db::providers(conn)?;
    let mut rows = Vec::new();
    let mut bad = false;
    for a in db::accounts(conn)? {
        let Some(p) = providers.iter().find(|p| p.id == a.provider_id) else {
            continue;
        };
        let source = Source::from_db(&a.source_kind, &a.source_value)?;
        let check = source
            .resolve(&a.name)
            .and_then(|token| provider::build(p)?.whoami(&token));
        let state = match check {
            Ok(who) if who.login.eq_ignore_ascii_case(&a.login) => "ok".to_string(),
            Ok(who) => format!("token now belongs to {}", who.login),
            Err(e) => format!("{e:#}"),
        };
        bad |= state != "ok";
        rows.push((a, p.host.clone(), source.describe(), state));
    }
    if as_json {
        let out: Vec<_> = rows
            .iter()
            .map(|(a, host, source, state)| {
                json!({"name": a.name, "host": host, "login": a.login, "source": source, "status": state})
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else if rows.is_empty() {
        println!("No accounts yet. Add one with `weir auth login <host>`.");
    } else {
        for (a, host, source, state) in &rows {
            println!(
                "{:<14} {:<18} {:<22} {:<8} {}",
                a.name, host, a.login, state, source
            );
        }
    }
    Ok(if bad { 1 } else { 0 })
}

fn auth_logout(conn: &Connection, name: &str) -> Result<()> {
    let Some(a) = db::account_by_name(conn, name)? else {
        bail!("there is no account named {name:?}");
    };
    let users: Vec<String> = db::tracked(conn)?
        .into_iter()
        .filter(|t| t.account_id == Some(a.id))
        .map(|t| t.owner)
        .collect();
    if !users.is_empty() {
        bail!(
            "account {name:?} is still used to track {}; untrack those or move them to another account first",
            users.join(", ")
        );
    }
    if a.source_kind == "keychain" {
        secret::keychain_delete(&a.name)?;
    }
    conn.execute("DELETE FROM accounts WHERE id = ?1", [a.id])?;
    println!("Removed account {name:?}.");
    Ok(())
}

/// Answers git's request for credentials with the account's token.
fn credential(conn: &Connection, account: &str, operation: Option<String>) -> Result<()> {
    // Git sends the request on stdin; read it so git never sees a closed pipe.
    let mut request = String::new();
    let _ = std::io::stdin().read_to_string(&mut request);
    if operation.as_deref() != Some("get") {
        return Ok(());
    }
    let Some(a) = db::account_by_name(conn, account)? else {
        bail!("there is no account named {account:?}");
    };
    let token = Source::from_db(&a.source_kind, &a.source_value)?.resolve(&a.name)?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "username={}", a.login)?;
    writeln!(out, "password={token}")?;
    Ok(())
}

// ----------------------------------------------------------------- track

#[allow(clippy::too_many_arguments)]
fn track(
    conn: &Connection,
    target: &str,
    account: Option<String>,
    include: Vec<String>,
    exclude: Vec<String>,
    forks: bool,
    archived: bool,
    max_size: Option<i64>,
) -> Result<()> {
    let (p, owner, repo) = target::parse_full(conn, target)?;
    if max_size.is_some_and(|mb| mb <= 0) {
        bail!("--max-size must be a positive number of megabytes");
    }
    let prov = provider::build(&p)?;
    for pattern in include.iter().chain(&exclude) {
        glob::Pattern::new(pattern).with_context(|| format!("{pattern:?} is not a valid glob"))?;
    }
    let existing = db::tracked(conn)?;
    let same_owner: Vec<&db::TrackedRow> = existing
        .iter()
        .filter(|t| t.provider_id == p.id && t.owner.eq_ignore_ascii_case(&owner))
        .collect();
    if !repo.is_empty() && same_owner.iter().any(|t| t.repo.is_empty()) {
        bail!(
            "all of {}/{owner} is already tracked, which includes {repo}",
            p.host
        );
    }
    let current = same_owner
        .iter()
        .find(|t| t.repo.eq_ignore_ascii_case(&repo))
        .copied();

    let account_id = if !prov.needs_account() {
        None
    } else {
        let candidates: Vec<db::AccountRow> = db::accounts(conn)?
            .into_iter()
            .filter(|a| a.provider_id == p.id)
            .collect();
        let names = || {
            candidates
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let chosen = match (&account, current.and_then(|t| t.account_id)) {
            (Some(name), _) => {
                candidates
                    .iter()
                    .find(|a| &a.name == name)
                    .with_context(|| {
                        if candidates.is_empty() {
                            format!(
                                "{} has no accounts; run `weir auth login {}`",
                                p.host, p.host
                            )
                        } else {
                            format!(
                                "{} has no account named {name:?} (it has: {})",
                                p.host,
                                names()
                            )
                        }
                    })?
                    .id
            }
            (None, Some(id)) => id,
            (None, None) => match candidates.len() {
                0 => bail!(
                    "{} has no accounts yet; run `weir auth login {}`",
                    p.host,
                    p.host
                ),
                1 => candidates[0].id,
                _ => bail!(
                    "{} has several accounts; pass --account ({})",
                    p.host,
                    names()
                ),
            },
        };
        Some(chosen)
    };

    if repo.is_empty() {
        // Tracking the whole owner takes over any single repositories tracked before.
        conn.execute(
            "DELETE FROM tracked WHERE provider_id = ?1 AND owner = ?2 AND repo <> ''",
            params![p.id, owner],
        )?;
    }
    conn.execute(
        "INSERT INTO tracked (provider_id, account_id, owner, repo, include, exclude, forks, archived, created_at, max_size_mb)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (provider_id, owner, repo) DO UPDATE SET
           account_id = excluded.account_id, include = excluded.include, exclude = excluded.exclude,
           forks = excluded.forks, archived = excluded.archived, max_size_mb = excluded.max_size_mb",
        params![
            p.id,
            account_id,
            owner,
            repo,
            serde_json::to_string(&include)?,
            serde_json::to_string(&exclude)?,
            forks,
            archived,
            util::now(),
            max_size
        ],
    )?;
    let through = match account_id {
        Some(id) => {
            let name: String =
                conn.query_row("SELECT name FROM accounts WHERE id = ?1", [id], |r| {
                    r.get(0)
                })?;
            format!(" through account {name:?}")
        }
        None => String::new(),
    };
    let verb = if current.is_some() {
        "Updated"
    } else {
        "Tracking"
    };
    println!(
        "{verb} {}{through}. Run `weir sync` to mirror it.",
        target.trim_matches('/')
    );
    Ok(())
}

fn untrack(conn: &Connection, target: &str) -> Result<()> {
    let (p, owner, repo) = target::parse_full(conn, target)?;
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM tracked WHERE provider_id = ?1 AND owner = ?2 AND repo = ?3",
            params![p.id, owner, repo],
            |r| r.get(0),
        )
        .optional()?;
    let Some(id) = id else {
        bail!("{target} is not tracked; see `weir status`");
    };
    let mirrors = conn.execute(
        "UPDATE repos SET state = 'untracked' WHERE tracked_id = ?1",
        [id],
    )?;
    conn.execute("DELETE FROM tracked WHERE id = ?1", [id])?;
    println!(
        "Stopped tracking {}. {mirrors} mirror(s) stay on disk; `weir prune` deletes them.",
        target.trim_matches('/')
    );
    Ok(())
}

// ----------------------------------------------------------------- prune

/// States whose mirrors `prune` deletes without being asked twice. Archived
/// mirrors need `--archived`; inaccessible ones are never pruned, because a
/// temporary loss of access must not cost a mirror.
const PRUNABLE: [&str; 4] = ["gone", "excluded", "untracked", "transferred"];

fn prune(
    conn: &Connection,
    dry_run: bool,
    yes: bool,
    archived: bool,
    unmanaged: bool,
) -> Result<()> {
    // Pruning and syncing must not touch the same mirror at once.
    let _lock = if dry_run {
        None
    } else {
        Some(sync::lock(true)?)
    };
    let root = settings::root(conn)?;
    let providers = db::providers(conn)?;
    let host_of = |id: i64| {
        providers
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.host.clone())
            .unwrap_or_default()
    };
    let wanted = |state: &str| PRUNABLE.contains(&state) || (archived && state == "archived");
    let all = db::repos(conn, "1 = 1", &[])?;
    let kept_paths: Vec<String> = all
        .iter()
        .filter(|r| !wanted(&r.state))
        .map(|r| {
            sync::path_of(&root, &host_of(r.provider_id), r)
                .to_string_lossy()
                .to_lowercase()
        })
        .collect();

    let mut doomed: Vec<(Option<i64>, std::path::PathBuf, String)> = Vec::new();
    for r in all.iter().filter(|r| wanted(&r.state)) {
        let host = host_of(r.provider_id);
        let dir = sync::mirror_path(
            &root,
            &host,
            r.disk_owner.as_deref().unwrap_or(&r.owner),
            r.disk_name.as_deref().unwrap_or(&r.name),
        );
        let label = format!("{host}/{}/{} ({})", r.owner, r.name, r.state);
        if !dir.exists() {
            // Nothing on disk: just forget the row, unless the provider still has it.
            if !dry_run && r.state != "archived" {
                conn.execute("DELETE FROM repos WHERE id = ?1", [r.id])?;
            }
            continue;
        }
        if kept_paths.contains(&dir.to_string_lossy().to_lowercase()) {
            continue;
        }
        let blockers = git::blockers(&dir);
        if blockers.is_empty() {
            doomed.push((Some(r.id), dir, label));
        } else {
            println!("kept      {label}: {}", blockers.join(", "));
        }
    }
    if unmanaged {
        // Of the clones weir does not manage, only a stale copy of a
        // repository that is mirrored elsewhere is safe to delete.
        for report in stray::classify(conn, &root, &stray::scan(conn, &root)?)? {
            let stray::Kind::Duplicate { repo, .. } = &report.kind else {
                continue;
            };
            let label = format!("{} (stale copy of {repo})", report.stray.label());
            let blockers = git::blockers(&report.stray.path);
            if blockers.is_empty() {
                doomed.push((None, report.stray.path.clone(), label));
            } else {
                println!("kept      {label}: {}", blockers.join(", "));
            }
        }
    }
    if doomed.is_empty() {
        println!("Nothing to prune.");
        return Ok(());
    }
    for (_, dir, label) in &doomed {
        println!(
            "{}  {label}  {}",
            if dry_run { "would delete" } else { "delete" },
            dir.display()
        );
    }
    if dry_run {
        return Ok(());
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            bail!("refusing to delete without confirmation; pass --yes");
        }
        print!("Delete these {} mirror(s)? [y/N] ", doomed.len());
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("Nothing deleted.");
            return Ok(());
        }
    }
    for (id, dir, _) in &doomed {
        std::fs::remove_dir_all(dir)
            .with_context(|| format!("could not delete {}", dir.display()))?;
        if let Some(parent) = dir.parent() {
            let _ = std::fs::remove_dir(parent);
        }
        if let Some(id) = id {
            conn.execute("DELETE FROM repos WHERE id = ?1", [id])?;
        }
    }
    println!("Deleted {} mirror(s).", doomed.len());
    Ok(())
}

/// Explains every git repository under the root that weir does not manage.
fn doctor(conn: &Connection, as_json: bool) -> Result<()> {
    let root = settings::root(conn)?;
    let reports = stray::classify(conn, &root, &stray::scan(conn, &root)?)?;
    if as_json {
        let out: Vec<_> = reports
            .iter()
            .map(|r| {
                json!({
                    "path": r.stray.path,
                    "kind": r.kind.label(),
                    "explanation": r.kind.explain(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    if reports.is_empty() {
        println!(
            "Every git repository under {} is managed by weir.",
            root.display()
        );
        return Ok(());
    }
    println!(
        "{} git repositories under {} are not managed by weir. They are never synced.\n",
        reports.len(),
        root.display()
    );
    for r in &reports {
        println!("{:<13} {}", r.kind.label(), r.stray.path.display());
        println!("{:<13} {}", "", r.kind.explain());
    }
    if reports
        .iter()
        .any(|r| matches!(r.kind, stray::Kind::Duplicate { .. }))
    {
        println!("\n`weir prune --unmanaged` deletes the stale copies (kind: duplicate) that have nothing to lose.");
    }
    Ok(())
}

// ------------------------------------------------------------- reporting

/// Whether syncs are keeping up: the last success, and the recent record.
struct Health {
    last_ok_at: Option<String>,
    /// Hours since the last successful sync, when that is longer than allowed.
    stale_hours: Option<i64>,
    limit_hours: u64,
    recent_ok: usize,
    recent_bad: usize,
}

fn health(conn: &Connection) -> Result<Health> {
    let limit_hours = settings::number(conn, "stale_after_hours")?;
    let last_ok_at: Option<String> = conn
        .query_row(
            "SELECT finished_at FROM sync_runs WHERE ok = 1 ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    // With no success yet, the clock starts at the first attempt.
    let first_attempt: Option<String> = conn
        .query_row("SELECT MIN(started_at) FROM sync_runs", [], |r| r.get(0))
        .optional()?
        .flatten();
    let since = last_ok_at
        .as_deref()
        .or(first_attempt.as_deref())
        .and_then(util::parse_utc);
    let stale_hours = since
        .map(|t| (util::unix_now() - t) / 3_600)
        .filter(|hours| limit_hours > 0 && *hours >= limit_hours as i64);
    let recent: Vec<runs::RunRow> = runs::latest(conn, 8)?
        .into_iter()
        .filter(|r| r.verdict() != "unfinished")
        .take(7)
        .collect();
    Ok(Health {
        last_ok_at,
        stale_hours,
        limit_hours,
        recent_ok: recent.iter().filter(|r| r.verdict() == "ok").count(),
        recent_bad: recent.iter().filter(|r| r.verdict() != "ok").count(),
    })
}

fn status(conn: &Connection, target: Option<&str>, as_json: bool) -> Result<i32> {
    let scope = target::resolve(conn, target)?;
    let root = settings::root(conn)?;
    let providers = db::providers(conn)?;
    let host_of = |id: i64| {
        providers
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.host.clone())
            .unwrap_or_default()
    };
    let accounts = db::accounts(conn)?;
    let tracked: Vec<db::TrackedRow> = db::tracked(conn)?
        .into_iter()
        .filter(|t| scope.matches_tracked(t))
        .collect();
    let repos: Vec<db::RepoRow> = db::repos(conn, "1 = 1", &[])?
        .into_iter()
        .filter(|r| scope.matches_repo(r))
        .collect();
    let attention: Vec<&db::RepoRow> = repos
        .iter()
        .filter(|r| {
            matches!(r.state.as_str(), "active" | "archived")
                && matches!(r.last_result.as_deref(), Some("skipped" | "failed"))
        })
        .collect();
    let leftover = repos
        .iter()
        .filter(|r| PRUNABLE.contains(&r.state.as_str()))
        .count();
    let inaccessible = repos.iter().filter(|r| r.state == "inaccessible").count();
    let unmanaged = stray::scan(conn, &root)?.len();
    let running = watch::running(conn)?;
    let last = runs::latest(conn, 2)?
        .into_iter()
        .find(|r| running.as_ref().is_none_or(|live| live.id != r.id));
    let health = health(conn)?;
    let code = if health.stale_hours.is_some() { 1 } else { 0 };

    struct TrackedLine {
        name: String,
        account: Option<String>,
        repos: usize,
        pending: usize,
        archived: usize,
        max_size_mb: Option<i64>,
    }
    let tracked_lines: Vec<TrackedLine> = tracked
        .iter()
        .map(|t| {
            let mine = |state: &str| -> Vec<&db::RepoRow> {
                repos
                    .iter()
                    .filter(|r| r.tracked_id == Some(t.id) && r.state == state)
                    .collect()
            };
            let active = mine("active");
            TrackedLine {
                name: if t.repo.is_empty() {
                    format!("{}/{}", host_of(t.provider_id), t.owner)
                } else {
                    format!("{}/{}/{}", host_of(t.provider_id), t.owner, t.repo)
                },
                account: t
                    .account_id
                    .and_then(|id| accounts.iter().find(|a| a.id == id))
                    .map(|a| a.name.clone()),
                repos: active.len(),
                pending: active.iter().filter(|r| r.last_result.is_none()).count(),
                archived: mine("archived").len(),
                max_size_mb: t.max_size_mb,
            }
        })
        .collect();

    if as_json {
        let out = json!({
            "root": root,
            "accounts": accounts.iter().map(|a| json!({
                "name": a.name, "host": host_of(a.provider_id), "login": a.login,
            })).collect::<Vec<_>>(),
            "tracked": tracked_lines.iter().map(|t| json!({
                "target": t.name, "account": t.account, "repos": t.repos,
                "not_synced": t.pending, "archived": t.archived, "max_size_mb": t.max_size_mb,
            })).collect::<Vec<_>>(),
            "running": match &running {
                Some(run) => json!({
                    "run_id": run.id, "started_at": run.started_at, "phase": run.phase,
                    "done": run.done, "planned": run.planned,
                    "counts": runs::live_counts(conn, run)?,
                    "current": run.current().iter().map(|(repo, seconds)| json!({
                        "repo": repo, "seconds": seconds,
                    })).collect::<Vec<_>>(),
                }),
                None => serde_json::Value::Null,
            },
            "last_sync": last.as_ref().map(|run| json!({
                "run_id": run.id, "started_at": run.started_at, "finished_at": run.finished_at,
                "seconds": run.seconds, "result": run.verdict(), "counts": run.counts(),
                "problems": run.problems(),
            })),
            "daemon": { "running": daemon::is_running(), "status": daemon::describe(conn)? },
            "health": {
                "last_ok_at": health.last_ok_at, "stale": health.stale_hours.is_some(),
                "stale_after_hours": health.limit_hours,
                "recent_ok": health.recent_ok, "recent_with_problems": health.recent_bad,
            },
            "needs_attention": attention.iter().map(|r| json!({
                "repo": format!("{}/{}/{}", host_of(r.provider_id), r.owner, r.name),
                "result": r.last_result, "message": r.last_message,
            })).collect::<Vec<_>>(),
            "prunable": leftover,
            "inaccessible": inaccessible,
            "unmanaged": unmanaged,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(code);
    }

    println!("Mirrors in {}", root.display());
    println!("\nAccounts");
    if accounts.is_empty() {
        println!("  none; add one with `weir auth login <host>`");
    }
    for a in &accounts {
        println!(
            "  {:<14} {:<18} {}",
            a.name,
            host_of(a.provider_id),
            a.login
        );
    }
    println!("\nTracked");
    if tracked_lines.is_empty() {
        println!("  nothing; start with `weir track <host>/<owner>`");
    }
    for t in &tracked_lines {
        let mut line = format!("  {:<40} {} repos", t.name, t.repos);
        if t.archived > 0 {
            line.push_str(&format!(" (+{} archived)", t.archived));
        }
        if let Some(a) = &t.account {
            line.push_str(&format!(", account {a}"));
        }
        if let Some(mb) = t.max_size_mb {
            line.push_str(&format!(", up to {mb} MB each"));
        }
        if t.pending > 0 {
            line.push_str(&format!(", {} not synced yet", t.pending));
        }
        println!("{line}");
    }

    println!();
    if let Some(line) = daemon::describe(conn)? {
        println!("{line}");
    } else if service::definition_path().exists() {
        println!(
            "Daemon not running, though its service is installed; `weir service start` starts it."
        );
    }
    if let Some(run) = &running {
        println!("Sync running: {}", watch::progress(conn, run)?);
    }
    let ago = |at: &str| {
        util::parse_utc(at)
            .map(|t| format!(" ({} ago)", util::ago(util::unix_now() - t)))
            .unwrap_or_default()
    };
    match &last {
        Some(run) => match run.verdict() {
            "ok" | "problems" => println!(
                "Last sync {}{}, took {}s: {}",
                run.started_at,
                ago(&run.started_at),
                run.seconds.unwrap_or(0),
                sync::summarize(&run.counts())
            ),
            "failed" => println!(
                "Last sync {}{} could not run: {}",
                run.started_at,
                ago(&run.started_at),
                run.problems().join("; ")
            ),
            _ => println!(
                "Last sync {}{} was interrupted before it finished.",
                run.started_at,
                ago(&run.started_at)
            ),
        },
        None if running.is_none() => println!("No sync has run yet."),
        None => {}
    }
    if let Some(run) = &last {
        for p in run
            .problems()
            .iter()
            .filter(|_| run.verdict() == "problems")
        {
            println!("  problem: {p}");
        }
    }
    if health.recent_ok + health.recent_bad > 1 {
        println!(
            "Last {} runs: {} ok, {} with problems",
            health.recent_ok + health.recent_bad,
            health.recent_ok,
            health.recent_bad
        );
    }
    if let Some(hours) = health.stale_hours {
        match &health.last_ok_at {
            Some(at) => println!(
                "\nWARNING: no sync has succeeded since {at} ({hours}h ago; the limit is {}h).",
                health.limit_hours
            ),
            None => println!(
                "\nWARNING: no sync has succeeded yet, {hours}h after the first attempt (the limit is {}h).",
                health.limit_hours
            ),
        }
    }
    if !attention.is_empty() {
        println!("\nNeeds attention");
        for r in &attention {
            println!(
                "  {:<8} {}/{}/{}  {}",
                r.last_result.as_deref().unwrap_or(""),
                host_of(r.provider_id),
                r.owner,
                r.name,
                r.last_message.as_deref().unwrap_or("")
            );
        }
    }
    if leftover > 0 {
        println!(
            "\n{leftover} mirror(s) are gone, excluded, transferred or untracked; `weir prune` deletes them."
        );
    }
    if inaccessible > 0 {
        println!(
            "\n{inaccessible} mirror(s) are of repositories this account can no longer read; they are kept (`weir list --all`)."
        );
    }
    if unmanaged > 0 {
        println!(
            "\n{unmanaged} git repositories under the root are not managed by weir; `weir doctor` explains them."
        );
    }
    Ok(code)
}

fn list(
    conn: &Connection,
    target: Option<&str>,
    archived: bool,
    all: bool,
    as_json: bool,
) -> Result<()> {
    let scope = target::resolve(conn, target)?;
    let root = settings::root(conn)?;
    let providers = db::providers(conn)?;
    let mut out = Vec::new();
    for r in db::repos(conn, "1 = 1", &[])? {
        let shown = all || r.state == "active" || (archived && r.state == "archived");
        if !scope.matches_repo(&r) || !shown {
            continue;
        }
        let host = providers
            .iter()
            .find(|p| p.id == r.provider_id)
            .map(|p| p.host.as_str())
            .unwrap_or_default();
        let name = format!("{host}/{}/{}", r.owner, r.name);
        if as_json {
            out.push(json!({
                "repo": name,
                "path": sync::path_of(&root, host, &r),
                "default_branch": r.default_branch,
                "head": r.head,
                "state": r.state,
                "last_synced_at": r.last_synced_at,
                "last_result": r.last_result,
            }));
        } else if r.state != "active" {
            println!("{name}  ({})", r.state);
        } else {
            println!("{name}");
        }
    }
    if as_json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    }
    Ok(())
}

fn path(conn: &Connection, repo: &str) -> Result<()> {
    let r = target::one_repo(conn, repo)?;
    let host: String = conn.query_row(
        "SELECT host FROM providers WHERE id = ?1",
        [r.provider_id],
        |row| row.get(0),
    )?;
    let dir = sync::path_of(&settings::root(conn)?, &host, &r);
    if !dir.exists() {
        bail!(
            "{host}/{}/{} has not been synced yet; run `weir sync`",
            r.owner,
            r.name
        );
    }
    println!("{}", dir.display());
    Ok(())
}

fn log(conn: &Connection, count: usize, all: bool, slow: bool, as_json: bool) -> Result<()> {
    let runs = runs::latest(conn, count)?;
    let mut out = Vec::new();
    for run in &runs {
        let mut changes: Vec<sync::Event> = runs::events(conn, run.id, 0)?
            .into_iter()
            .map(|(_, e)| e)
            .filter(|e| all || slow || e.action != "unchanged")
            .collect();
        if slow {
            changes.sort_by_key(|e| std::cmp::Reverse(e.millis.unwrap_or(0)));
            changes.truncate(10);
        }
        if as_json {
            out.push(json!({
                "run_id": run.id, "started_at": run.started_at, "finished_at": run.finished_at,
                "seconds": run.seconds, "listing_seconds": run.listing_seconds,
                "result": run.verdict(), "counts": run.counts(), "problems": run.problems(),
                "changes": changes,
            }));
            continue;
        }
        let mut header = format!("run {}  {}  {}", run.id, run.started_at, run.verdict());
        if let Some(s) = run.seconds {
            header.push_str(&format!("  {s}s"));
            if let Some(listing) = run.listing_seconds {
                header.push_str(&format!(" ({listing}s listing)"));
            }
        }
        if run.finished_at.is_some() && run.verdict() != "interrupted" {
            header.push_str(&format!("  {}", sync::summarize(&run.counts())));
        }
        println!("{header}");
        for p in run.problems() {
            println!("  problem   {p}");
        }
        for e in &changes {
            if slow {
                println!(
                    "  {:>7.1}s  {}",
                    e.millis.unwrap_or(0) as f64 / 1000.0,
                    sync::describe(e)
                );
            } else {
                println!("  {}", sync::describe(e));
            }
        }
    }
    if as_json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else if runs.is_empty() {
        println!("No sync has run yet.");
    }
    Ok(())
}

// ----------------------------------------------------------------- setup

fn provider_add(conn: &Connection, kind: &str, host: &str, api_url: Option<String>) -> Result<()> {
    if host.is_empty() || host.contains('/') {
        bail!("{host:?} is not a host name");
    }
    let api_url = match (kind, api_url) {
        (_, Some(url)) => url,
        ("github", None) => format!("https://{host}/api/v3"),
        ("gitlab", None) => format!("https://{host}/api/v4"),
        ("local", None) => {
            bail!("a local provider needs --api-url <directory of bare repositories>")
        }
        ("bitbucket", None) => bail!("a self-hosted bitbucket needs --api-url"),
        (other, None) => {
            bail!("unknown provider kind {other:?}; use github, gitlab, bitbucket or local")
        }
    };
    if !matches!(kind, "github" | "gitlab" | "bitbucket" | "local") {
        bail!("unknown provider kind {kind:?}; use github, gitlab, bitbucket or local");
    }
    if db::provider_by_host(conn, host)?.is_some() {
        bail!("{host} is already a provider");
    }
    conn.execute(
        "INSERT INTO providers (kind, host, api_url) VALUES (?1, ?2, ?3)",
        params![kind, host, api_url],
    )?;
    println!("Added {kind} provider {host}.");
    Ok(())
}

fn config_list(conn: &Connection, as_json: bool) -> Result<()> {
    if as_json {
        let mut out = serde_json::Map::new();
        for s in &settings::SETTINGS {
            out.insert(s.key.to_string(), json!(settings::get(conn, s.key)?));
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    for s in &settings::SETTINGS {
        let value = settings::get(conn, s.key)?;
        let shown = if value.is_empty() {
            "(not set)"
        } else {
            &value
        };
        println!("{:<18} {shown}", s.key);
        println!("{:<18} {}", "", s.help);
    }
    Ok(())
}

fn config_get(conn: &Connection, key: &str) -> Result<()> {
    if key == "root" {
        println!("{}", settings::root(conn)?.display());
    } else {
        println!("{}", settings::get(conn, key)?);
    }
    Ok(())
}

fn config_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    settings::set(conn, key, value)?;
    if key == "root" {
        println!("Mirrors now live in {}.", settings::root(conn)?.display());
    } else {
        println!("{key} = {value}");
    }
    Ok(())
}
