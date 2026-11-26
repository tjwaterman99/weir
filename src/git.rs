//! Everything weir does to a mirror on disk, by running `git`.
//!
//! A mirror is a plain clone that stays on a detached HEAD at
//! `origin/<default branch>`, with its tracked files read-only. Nothing here
//! ever resets, cleans or stashes: a mirror that has been modified is reported
//! and left exactly as it is.

use anyhow::{bail, Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn command(dir: Option<&Path>) -> Command {
    let mut c = Command::new("git");
    if let Some(d) = dir {
        c.arg("-C").arg(d);
    }
    c.env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env("PATH", child_path())
        .stdin(Stdio::null());
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        c.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    c
}

/// PATH for git, with weir's own directory first, so the credential helper
/// written into each mirror (`!weir credential ...`) is found even when weir
/// runs from a scheduler with a minimal PATH.
fn child_path() -> OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        dirs.push(dir);
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

fn run(mut c: Command, what: &str) -> Result<String> {
    let out = c.output().context("could not run git; is it installed?")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err: Vec<&str> = err
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        bail!("git {what} failed: {}", err.join("; "));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs git and returns its standard output. Fails when git does.
pub fn out(dir: Option<&Path>, args: &[&str]) -> Result<String> {
    let mut c = command(dir);
    c.args(args);
    run(c, args.first().copied().unwrap_or(""))
}

/// Runs git and returns its trimmed output, or `None` when git fails.
pub fn opt(dir: Option<&Path>, args: &[&str]) -> Option<String> {
    out(dir, args).ok().map(|s| s.trim().to_string())
}

fn helper(account: &str) -> String {
    format!("!weir credential --account {account}")
}

/// Git settings that stop a stalled connection from hanging a sync forever.
const NETWORK: [&str; 4] = [
    "-c",
    "http.lowSpeedLimit=1000",
    "-c",
    "http.lowSpeedTime=60",
];

/// Reduces a remote URL to `host/owner/name` so two spellings of the same
/// repository compare equal (https, ssh, scp-style, with or without `.git`).
pub fn normalize_url(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    let u = u.strip_suffix(".git").unwrap_or(u);
    let rest = if let Some((_, rest)) = u.split_once("://") {
        let authority_end = rest.find('/').unwrap_or(rest.len());
        match rest[..authority_end].rfind('@') {
            Some(at) => rest[at + 1..].to_string(),
            None => rest.to_string(),
        }
    } else if let Some((host, path)) = u.split_once(':').filter(|(h, _)| !h.contains('/')) {
        let host = host.rsplit('@').next().unwrap_or(host);
        format!("{host}/{}", path.trim_start_matches('/'))
    } else {
        u.to_string()
    };
    rest.to_lowercase()
}

#[derive(Debug, Clone)]
pub struct Job {
    pub repo_id: i64,
    /// `host/owner/name`, for messages.
    pub repo: String,
    pub path: PathBuf,
    pub clone_url: String,
    pub default_branch: String,
    /// The account whose token git should use, when the provider needs one.
    pub account: Option<String>,
    /// Whether to contact the remote. False when only the local state needs fixing.
    pub fetch: bool,
    /// Whether weir has synced this mirror before.
    pub known: bool,
    /// Where the mirror used to be, when the repository was renamed upstream.
    pub move_from: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Cloned,
    Adopted,
    Updated,
    Healed,
    Moved,
    Unchanged,
    Skipped,
    Failed,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Cloned => "cloned",
            Action::Adopted => "adopted",
            Action::Updated => "updated",
            Action::Healed => "healed",
            Action::Moved => "moved",
            Action::Unchanged => "unchanged",
            Action::Skipped => "skipped",
            Action::Failed => "failed",
        }
    }

    pub fn is_problem(self) -> bool {
        matches!(self, Action::Skipped | Action::Failed)
    }
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub action: Action,
    pub old_head: Option<String>,
    pub new_head: Option<String>,
    pub notes: Vec<String>,
}

impl Outcome {
    pub fn message(&self) -> Option<String> {
        if self.notes.is_empty() {
            None
        } else {
            Some(self.notes.join("; "))
        }
    }
}

/// Brings one mirror in line with its remote. Never panics and never fails:
/// a problem comes back as a `Failed` or `Skipped` outcome.
pub fn sync_mirror(job: &Job) -> Outcome {
    match sync_inner(job) {
        Ok(outcome) => outcome,
        Err(e) => Outcome {
            action: Action::Failed,
            old_head: None,
            new_head: None,
            notes: vec![format!("{e:#}")],
        },
    }
}

fn sync_inner(job: &Job) -> Result<Outcome> {
    let path = job.path.as_path();
    let mut notes = Vec::new();
    let mut moved = false;

    if let Some(from) = &job.move_from {
        if from.exists() && !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(from, path).with_context(|| {
                format!("could not move {} to {}", from.display(), path.display())
            })?;
            // Linked worktrees point back at the old location; fix them.
            let _ = out(Some(path), &["worktree", "repair"]);
            if let Some(parent) = from.parent() {
                let _ = std::fs::remove_dir(parent);
            }
            notes.push(format!("moved from {}", from.display()));
            moved = true;
        }
    }

    if !path.exists() {
        return clone(job, notes);
    }
    if !path.join(".git").is_dir() {
        bail!("{} exists and is not a git repository", path.display());
    }

    let Some(origin) = opt(Some(path), &["config", "--get", "remote.origin.url"]) else {
        bail!("{} has no origin remote", path.display());
    };
    if normalize_url(&origin) != normalize_url(&job.clone_url) {
        if job.known || moved {
            out(Some(path), &["remote", "set-url", "origin", &job.clone_url])?;
        } else {
            bail!(
                "{} is a clone of {origin}, not of {}",
                path.display(),
                job.clone_url
            );
        }
    }

    configure(path, job.account.as_deref())?;

    if job.fetch {
        let mut args = NETWORK.to_vec();
        args.extend(["fetch", "--quiet", "--prune", "origin"]);
        out(Some(path), &args)?;
    }

    let branch = job.default_branch.as_str();
    let old_head = opt(
        Some(path),
        &["rev-parse", "-q", "--verify", "HEAD^{commit}"],
    );
    let attached = opt(Some(path), &["symbolic-ref", "-q", "--short", "HEAD"]);
    let remote_ref = format!("refs/remotes/origin/{branch}^{{commit}}");
    let Some(target) = opt(Some(path), &["rev-parse", "-q", "--verify", &remote_ref]) else {
        notes.push(format!("origin has no commits on {branch}"));
        return Ok(Outcome {
            action: first_time_or(job, moved, Action::Unchanged),
            new_head: old_head.clone(),
            old_head,
            notes,
        });
    };
    let _ = out(Some(path), &["remote", "set-head", "origin", branch]);

    let in_place = attached.is_none() && old_head.as_deref() == Some(target.as_str());
    if !in_place {
        let status = out(
            Some(path),
            &["status", "--porcelain", "--untracked-files=no"],
        )?;
        if !status.trim().is_empty() {
            notes.push("has modified files; left as it is".to_string());
            return Ok(Outcome {
                action: Action::Skipped,
                new_head: old_head.clone(),
                old_head,
                notes,
            });
        }
        out(Some(path), &["checkout", "--quiet", "--detach", &target])?;
        if let Some(b) = &attached {
            notes.push(format!("HEAD was on branch {b}"));
        }
    }

    // A mirror has no use for a local copy of the default branch. Remove it
    // when that loses nothing; keep it, and say so, when it holds commits.
    let local = format!("refs/heads/{branch}");
    if opt(Some(path), &["rev-parse", "-q", "--verify", &local]).is_some() {
        let merged = command(Some(path))
            .args(["merge-base", "--is-ancestor", &local, &target])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if merged {
            let _ = out(Some(path), &["branch", "--quiet", "-D", branch]);
        } else {
            notes.push(format!(
                "local branch {branch} has commits that are not on origin; kept"
            ));
        }
    }
    if !job.known && opt(Some(path), &["rev-parse", "-q", "--verify", "refs/stash"]).is_some() {
        notes.push("has stashed changes; kept".to_string());
    }

    let action = if !job.known {
        Action::Adopted
    } else if attached.is_some() || (!in_place && !job.fetch) {
        Action::Healed
    } else if !in_place {
        Action::Updated
    } else if moved {
        Action::Moved
    } else {
        Action::Unchanged
    };
    if action != Action::Unchanged {
        make_read_only(path)?;
    }
    Ok(Outcome {
        action,
        old_head,
        new_head: Some(target),
        notes,
    })
}

fn first_time_or(job: &Job, moved: bool, otherwise: Action) -> Action {
    if !job.known {
        Action::Adopted
    } else if moved {
        Action::Moved
    } else {
        otherwise
    }
}

fn clone(job: &Job, mut notes: Vec<String>) -> Result<Outcome> {
    let path = job.path.as_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let mut c = command(None);
    c.args(NETWORK);
    if let Some(account) = &job.account {
        // An empty value first clears helpers from the user's own git config.
        c.args(["-c", "credential.helper="]);
        c.arg("-c")
            .arg(format!("credential.helper={}", helper(account)));
    }
    c.args(["clone", "--quiet", "--no-checkout"])
        .arg(&job.clone_url)
        .arg(path);
    run(c, "clone")?;
    configure(path, job.account.as_deref())?;

    let branch = job.default_branch.as_str();
    let remote_ref = format!("refs/remotes/origin/{branch}^{{commit}}");
    let Some(target) = opt(Some(path), &["rev-parse", "-q", "--verify", &remote_ref]) else {
        notes.push(format!("origin has no commits on {branch}"));
        return Ok(Outcome {
            action: Action::Cloned,
            old_head: None,
            new_head: None,
            notes,
        });
    };
    out(Some(path), &["checkout", "--quiet", "--detach", &target])?;
    let _ = out(Some(path), &["remote", "set-head", "origin", branch]);
    let heads = out(
        Some(path),
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?;
    for head in heads.lines().filter(|l| !l.is_empty()) {
        let _ = out(Some(path), &["branch", "--quiet", "-D", head]);
    }
    make_read_only(path)?;
    Ok(Outcome {
        action: Action::Cloned,
        old_head: None,
        new_head: Some(target),
        notes,
    })
}

/// Points the mirror's credential helper at weir, for the given account.
/// Worktrees share this config, so pushes from them use the same account.
fn configure(path: &Path, account: Option<&str>) -> Result<()> {
    let Some(account) = account else {
        return Ok(());
    };
    let want = helper(account);
    let current = out(
        Some(path),
        &["config", "--local", "--get-all", "credential.helper"],
    )
    .unwrap_or_default();
    if current == format!("\n{want}\n") {
        return Ok(());
    }
    let _ = out(
        Some(path),
        &["config", "--local", "--unset-all", "credential.helper"],
    );
    out(Some(path), &["config", "--local", "credential.helper", ""])?;
    out(
        Some(path),
        &["config", "--local", "--add", "credential.helper", &want],
    )?;
    Ok(())
}

/// Removes the write bit from every tracked file. Directories stay writable,
/// because git replaces files when it updates the checkout. Git only records
/// the executable bit, so this never shows up as a modification.
fn make_read_only(path: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    let mut c = command(Some(path));
    c.args(["ls-files", "-z"]);
    let out = c.output().context("could not run git")?;
    if !out.status.success() {
        bail!("git ls-files failed in {}", path.display());
    }
    for name in out.stdout.split(|b| *b == 0).filter(|n| !n.is_empty()) {
        let file = path.join(std::ffi::OsStr::from_bytes(name));
        // Symlinks are skipped: chmod would follow them out of the mirror.
        let Ok(meta) = std::fs::symlink_metadata(&file) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let mode = meta.permissions().mode();
        if mode & 0o222 != 0 {
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode & !0o222))?;
        }
    }
    Ok(())
}

/// Reasons a mirror must not be deleted. Empty when deleting loses nothing.
pub fn blockers(path: &Path) -> Vec<String> {
    let mut found = Vec::new();
    if !path.join(".git").is_dir() {
        found.push("is not a git repository".to_string());
        return found;
    }
    let dir = Some(path);
    match out(dir, &["worktree", "list", "--porcelain"]) {
        Ok(list) => {
            let count = list.lines().filter(|l| l.starts_with("worktree ")).count();
            if count > 1 {
                found.push(format!("has {} worktrees", count - 1));
            }
        }
        Err(_) => found.push("could not list its worktrees".to_string()),
    }
    match out(dir, &["status", "--porcelain"]) {
        Ok(s) if s.trim().is_empty() => {}
        Ok(_) => found.push("has modified or untracked files".to_string()),
        Err(_) => found.push("could not read its status".to_string()),
    }
    if opt(dir, &["rev-parse", "-q", "--verify", "refs/stash"]).is_some() {
        found.push("has stashed changes".to_string());
    }
    match out(
        dir,
        &["log", "--branches", "--not", "--remotes", "--oneline", "-1"],
    ) {
        Ok(s) if s.trim().is_empty() => {}
        Ok(_) => found.push("has commits that were never pushed".to_string()),
        Err(_) => found.push("could not read its branches".to_string()),
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_remote_urls() {
        let want = "github.com/acme/weir";
        for url in [
            "https://github.com/acme/weir.git",
            "https://github.com/Acme/Weir",
            "https://user:token@github.com/acme/weir.git/",
            "git@github.com:acme/weir.git",
            "ssh://git@github.com/acme/weir",
        ] {
            assert_eq!(normalize_url(url), want, "{url}");
        }
        assert_eq!(normalize_url("/srv/git/acme/app.git"), "/srv/git/acme/app");
        assert_ne!(
            normalize_url("https://github.com/acme/weir"),
            normalize_url("https://github.com/acme/other")
        );
    }
}
