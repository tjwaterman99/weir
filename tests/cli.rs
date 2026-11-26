//! End-to-end tests: they run the real `weir` binary against throwaway git
//! repositories in a temp directory. Nothing touches the network, the user's
//! git config, `~/Repos` or `~/.weir`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        let env = Env {
            dir: tempfile::tempdir().unwrap(),
        };
        for d in ["home", "userhome", "root", "up", "work"] {
            std::fs::create_dir_all(env.p(d)).unwrap();
        }
        std::fs::write(
            env.p("gitconfig"),
            "[user]\n\tname = Test\n\temail = test@example.com\n[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n",
        )
        .unwrap();
        env.ok(&["config", "set", "root", env.p("root").to_str().unwrap()]);
        env.ok(&[
            "provider",
            "add",
            "local",
            "local.test",
            "--api-url",
            env.p("up").to_str().unwrap(),
        ]);
        env
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn isolate(&self, c: &mut Command) {
        c.env("HOME", self.p("userhome"))
            .env("WEIR_HOME", self.p("home"))
            .env("GIT_CONFIG_GLOBAL", self.p("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
    }

    fn weir_env(&self, vars: &[(&str, &str)], args: &[&str]) -> Run {
        let mut c = Command::new(env!("CARGO_BIN_EXE_weir"));
        self.isolate(&mut c);
        c.envs(vars.iter().copied()).args(args);
        let out = c.output().unwrap();
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn weir(&self, args: &[&str]) -> Run {
        self.weir_env(&[], args)
    }

    /// Runs weir and requires it to succeed.
    fn ok(&self, args: &[&str]) -> Run {
        let run = self.weir(args);
        assert_eq!(
            run.code, 0,
            "weir {args:?} failed\nstdout: {}\nstderr: {}",
            run.stdout, run.stderr
        );
        run
    }

    fn try_git(&self, dir: &Path, args: &[&str]) -> Option<String> {
        let mut c = Command::new("git");
        self.isolate(&mut c);
        let out = c.arg("-C").arg(dir).args(args).output().unwrap();
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        self.try_git(dir, args)
            .unwrap_or_else(|| panic!("git {args:?} failed in {}", dir.display()))
    }

    fn upstream(&self, owner: &str, name: &str) -> PathBuf {
        self.p("up").join(owner).join(format!("{name}.git"))
    }

    fn work(&self, owner: &str, name: &str) -> PathBuf {
        self.p("work").join(format!("{owner}-{name}"))
    }

    fn mirror(&self, owner: &str, name: &str) -> PathBuf {
        self.p("root").join("local.test").join(owner).join(name)
    }

    /// Creates an upstream repository with one commit.
    fn make_repo(&self, owner: &str, name: &str) -> PathBuf {
        let bare = self.upstream(owner, name);
        std::fs::create_dir_all(&bare).unwrap();
        self.git(&bare, &["init", "-q", "--bare", "-b", "main"]);
        let work = self.work(owner, name);
        self.git(
            &self.p("work"),
            &[
                "clone",
                "-q",
                bare.to_str().unwrap(),
                work.to_str().unwrap(),
            ],
        );
        self.push(owner, name, "README.md", "one\n");
        bare
    }

    /// Commits a file upstream.
    fn push(&self, owner: &str, name: &str, file: &str, content: &str) {
        let work = self.work(owner, name);
        std::fs::write(work.join(file), content).unwrap();
        self.git(&work, &["add", "."]);
        self.git(&work, &["commit", "-q", "-m", &format!("change {file}")]);
        self.git(&work, &["push", "-q", "origin", "HEAD:main"]);
    }

    fn upstream_head(&self, owner: &str, name: &str) -> String {
        self.git(&self.upstream(owner, name), &["rev-parse", "main"])
    }

    fn assert_mirror(&self, owner: &str, name: &str) {
        let m = self.mirror(owner, name);
        assert!(m.join(".git").is_dir(), "{} is not a mirror", m.display());
        assert!(
            self.try_git(&m, &["symbolic-ref", "-q", "HEAD"]).is_none(),
            "HEAD is on a branch"
        );
        assert_eq!(
            self.git(&m, &["rev-parse", "HEAD"]),
            self.upstream_head(owner, name),
            "mirror is not at the upstream commit"
        );
        for file in self.git(&m, &["ls-files"]).lines() {
            let mode = std::fs::metadata(m.join(file))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o222, 0, "{file} is writable");
        }
        assert_eq!(
            self.git(&m, &["status", "--porcelain"]),
            "",
            "mirror is dirty"
        );
    }

    fn local_branches(&self, owner: &str, name: &str) -> Vec<String> {
        self.git(
            &self.mirror(owner, name),
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        )
        .lines()
        .map(String::from)
        .collect()
    }
}

#[test]
fn clones_then_updates_only_what_changed() {
    let env = Env::new();
    env.make_repo("acme", "app");
    env.make_repo("acme", "lib");
    env.ok(&["track", "local.test/acme"]);

    let first = env.ok(&["sync"]);
    assert!(first.stdout.contains("2 cloned"), "{}", first.stdout);
    env.assert_mirror("acme", "app");
    env.assert_mirror("acme", "lib");
    assert!(env.local_branches("acme", "app").is_empty());

    let second = env.ok(&["sync"]);
    assert!(second.stdout.contains("2 unchanged"), "{}", second.stdout);

    env.push("acme", "app", "README.md", "two\n");
    let third = env.ok(&["sync"]);
    assert!(
        third.stdout.contains("updated   local.test/acme/app"),
        "{}",
        third.stdout
    );
    assert!(
        third.stdout.contains("1 updated, 1 unchanged"),
        "{}",
        third.stdout
    );
    env.assert_mirror("acme", "app");
    assert_eq!(
        std::fs::read_to_string(env.mirror("acme", "app").join("README.md")).unwrap(),
        "two\n"
    );

    let log = env.ok(&["log"]);
    assert!(
        log.stdout.contains("cloned    local.test/acme/lib"),
        "{}",
        log.stdout
    );
    let status = env.ok(&["status"]);
    assert!(status.stdout.contains("2 repos"), "{}", status.stdout);
}

#[test]
fn puts_head_back_when_a_branch_was_checked_out() {
    let env = Env::new();
    env.make_repo("acme", "app");
    env.ok(&["track", "local.test/acme"]);
    env.ok(&["sync"]);

    let m = env.mirror("acme", "app");
    env.git(&m, &["checkout", "-q", "-b", "oops"]);
    let run = env.ok(&["sync"]);
    assert!(run.stdout.contains("healed"), "{}", run.stdout);
    assert!(
        run.stdout.contains("HEAD was on branch oops"),
        "{}",
        run.stdout
    );
    env.assert_mirror("acme", "app");
    // The branch itself is kept: nothing is ever thrown away.
    assert_eq!(env.local_branches("acme", "app"), ["oops"]);
}

#[test]
fn leaves_a_modified_mirror_alone() {
    let env = Env::new();
    env.make_repo("acme", "app");
    env.ok(&["track", "local.test/acme"]);
    env.ok(&["sync"]);

    let file = env.mirror("acme", "app").join("README.md");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(&file, "my edit\n").unwrap();
    env.push("acme", "app", "README.md", "two\n");

    let run = env.weir(&["sync"]);
    assert_eq!(run.code, 1, "{}{}", run.stdout, run.stderr);
    assert!(run.stderr.contains("skipped"), "{}", run.stderr);
    assert!(run.stderr.contains("modified files"), "{}", run.stderr);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "my edit\n");
    let status = env.ok(&["status"]);
    assert!(
        status.stdout.contains("Needs attention"),
        "{}",
        status.stdout
    );

    env.git(
        &env.mirror("acme", "app"),
        &["checkout", "-q", "--", "README.md"],
    );
    let again = env.ok(&["sync"]);
    assert!(again.stdout.contains("updated"), "{}", again.stdout);
    env.assert_mirror("acme", "app");
}

#[test]
fn adopts_clones_that_already_exist() {
    let env = Env::new();
    let app = env.make_repo("acme", "app");
    let lib = env.make_repo("acme", "lib");
    let other = env.make_repo("acme", "other");
    env.make_repo("acme", "wrong");
    std::fs::create_dir_all(env.mirror("acme", "x").parent().unwrap()).unwrap();
    let parent = env.mirror("acme", "x").parent().unwrap().to_path_buf();

    // An ordinary clone sitting on its default branch.
    env.git(&parent, &["clone", "-q", app.to_str().unwrap(), "app"]);
    // A clone with a commit on main that was never pushed.
    env.git(&parent, &["clone", "-q", lib.to_str().unwrap(), "lib"]);
    let lib_mirror = env.mirror("acme", "lib");
    std::fs::write(lib_mirror.join("local.txt"), "mine\n").unwrap();
    env.git(&lib_mirror, &["add", "."]);
    env.git(&lib_mirror, &["commit", "-q", "-m", "local work"]);
    let local_commit = env.git(&lib_mirror, &["rev-parse", "main"]);
    // A directory that is a clone of some other repository.
    env.git(&parent, &["clone", "-q", other.to_str().unwrap(), "wrong"]);

    env.ok(&["track", "local.test/acme"]);
    let run = env.weir(&["sync"]);
    assert_eq!(run.code, 1, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout.contains("adopted   local.test/acme/app"),
        "{}",
        run.stdout
    );
    env.assert_mirror("acme", "app");
    assert!(env.local_branches("acme", "app").is_empty());

    assert!(
        run.stdout.contains("adopted   local.test/acme/lib"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("commits that are not on origin"),
        "{}",
        run.stdout
    );
    env.assert_mirror("acme", "lib");
    assert_eq!(env.git(&lib_mirror, &["rev-parse", "main"]), local_commit);

    assert!(
        run.stderr.contains("failed    local.test/acme/wrong"),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("is a clone of"), "{}", run.stderr);
}

#[test]
fn follows_a_rename_without_cloning_again() {
    let env = Env::new();
    let bare = env.make_repo("acme", "old");
    std::fs::write(bare.join("weir-id"), "42\n").unwrap();
    env.ok(&["track", "local.test/acme"]);
    env.ok(&["sync"]);
    std::fs::write(env.mirror("acme", "old").join("untracked.txt"), "keep me\n").unwrap();

    std::fs::rename(&bare, env.upstream("acme", "new")).unwrap();
    let run = env.ok(&["sync"]);
    assert!(
        run.stdout.contains("moved     local.test/acme/new"),
        "{}",
        run.stdout
    );
    assert!(!env.mirror("acme", "old").exists());
    let m = env.mirror("acme", "new");
    assert_eq!(
        std::fs::read_to_string(m.join("untracked.txt")).unwrap(),
        "keep me\n"
    );
    assert!(env
        .git(&m, &["config", "--get", "remote.origin.url"])
        .ends_with("new.git"));
    assert_eq!(env.ok(&["list"]).stdout.trim(), "local.test/acme/new");

    let again = env.ok(&["sync"]);
    assert!(again.stdout.contains("1 unchanged"), "{}", again.stdout);
}

#[test]
fn keeps_gone_mirrors_until_pruned() {
    let env = Env::new();
    env.make_repo("acme", "app");
    let gone = env.make_repo("acme", "gone");
    env.ok(&["track", "local.test/acme"]);
    env.ok(&["sync"]);

    std::fs::remove_dir_all(&gone).unwrap();
    let run = env.ok(&["sync"]);
    assert!(
        run.stdout.contains("gone      local.test/acme/gone"),
        "{}",
        run.stdout
    );
    let m = env.mirror("acme", "gone");
    assert!(m.exists(), "a gone mirror must stay on disk");
    assert_eq!(env.ok(&["list"]).stdout.trim(), "local.test/acme/app");

    // A mirror with a worktree is never deleted.
    let wt = env.p("work").join("wt");
    env.git(
        &m,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            wt.to_str().unwrap(),
        ],
    );
    let kept = env.ok(&["prune", "--yes"]);
    assert!(kept.stdout.contains("kept"), "{}", kept.stdout);
    assert!(kept.stdout.contains("worktree"), "{}", kept.stdout);
    assert!(m.exists());

    env.git(&m, &["worktree", "remove", wt.to_str().unwrap()]);
    let dry = env.ok(&["prune", "--dry-run"]);
    assert!(dry.stdout.contains("would delete"), "{}", dry.stdout);
    assert!(m.exists());
    // Without --yes and without a terminal, nothing is deleted.
    assert_eq!(env.weir(&["prune"]).code, 2);
    assert!(m.exists());

    let pruned = env.ok(&["prune", "--yes"]);
    assert!(
        pruned.stdout.contains("Deleted 1 mirror"),
        "{}",
        pruned.stdout
    );
    assert!(!m.exists());
    assert!(env.mirror("acme", "app").exists());
    assert!(!env.ok(&["list", "--all"]).stdout.contains("gone"));
}

#[test]
fn applies_filters() {
    let env = Env::new();
    env.make_repo("acme", "app");
    env.make_repo("acme", "app-docs");
    let archived = env.make_repo("acme", "legacy");
    std::fs::write(archived.join("weir-archived"), "").unwrap();
    let fork = env.make_repo("acme", "forked");
    std::fs::write(fork.join("weir-fork"), "").unwrap();

    env.ok(&["track", "local.test/acme"]);
    env.ok(&["sync"]);
    assert_eq!(
        env.ok(&["list"]).stdout.lines().collect::<Vec<_>>(),
        ["local.test/acme/app", "local.test/acme/app-docs"]
    );

    // Tightening the filter excludes a mirror but keeps it on disk.
    env.ok(&[
        "track",
        "local.test/acme",
        "--exclude",
        "*-docs",
        "--archived",
    ]);
    let run = env.ok(&["sync"]);
    assert!(
        run.stdout.contains("excluded  local.test/acme/app-docs"),
        "{}",
        run.stdout
    );
    assert_eq!(
        env.ok(&["list"]).stdout.lines().collect::<Vec<_>>(),
        ["local.test/acme/app", "local.test/acme/legacy"]
    );
    assert!(env.mirror("acme", "app-docs").exists());
}

#[test]
fn dry_run_changes_nothing() {
    let env = Env::new();
    env.make_repo("acme", "app");
    env.ok(&["track", "local.test/acme"]);
    env.ok(&["sync"]);
    let before = env.git(&env.mirror("acme", "app"), &["rev-parse", "HEAD"]);

    env.push("acme", "app", "README.md", "two\n");
    env.make_repo("acme", "new");
    let run = env.ok(&["sync", "--dry-run"]);
    assert!(
        run.stdout.contains("fetch     local.test/acme/app"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("clone     local.test/acme/new"),
        "{}",
        run.stdout
    );
    assert_eq!(
        env.git(&env.mirror("acme", "app"), &["rev-parse", "HEAD"]),
        before
    );
    assert!(!env.mirror("acme", "new").exists());
    assert_eq!(env.ok(&["list"]).stdout.trim(), "local.test/acme/app");
    assert!(env.ok(&["log"]).stdout.matches("run ").count() == 1);
}

#[test]
fn tracks_single_repositories_and_resolves_names() {
    let env = Env::new();
    env.make_repo("acme", "app");
    env.make_repo("acme", "lib");
    env.make_repo("other", "app");
    env.ok(&["track", "local.test/acme/app"]);
    env.ok(&["track", "local.test/other"]);
    env.ok(&["sync"]);
    assert!(!env.mirror("acme", "lib").exists());

    let path = env.ok(&["path", "acme/app"]);
    assert_eq!(PathBuf::from(path.stdout.trim()), env.mirror("acme", "app"));
    let ambiguous = env.weir(&["path", "app"]);
    assert_eq!(ambiguous.code, 2);
    assert!(
        ambiguous.stderr.contains("ambiguous"),
        "{}",
        ambiguous.stderr
    );
    assert_eq!(env.weir(&["path", "nope"]).code, 2);

    assert_eq!(
        env.ok(&["list", "other"]).stdout.trim(),
        "local.test/other/app"
    );
    let scoped = env.ok(&["sync", "local.test/other", "--all"]);
    assert!(scoped.stdout.contains("1 unchanged"), "{}", scoped.stdout);

    // Tracking the whole owner takes over from the single repository.
    assert_eq!(env.weir(&["track", "local.test/other/app"]).code, 2);
    env.ok(&["track", "local.test/acme"]);
    env.ok(&["sync"]);
    assert!(env.mirror("acme", "lib").exists());

    let untracked = env.ok(&["untrack", "local.test/other"]);
    assert!(
        untracked.stdout.contains("1 mirror(s) stay on disk"),
        "{}",
        untracked.stdout
    );
    assert!(env.mirror("other", "app").exists());
    assert!(!env.ok(&["list"]).stdout.contains("other"));
    assert_eq!(env.weir(&["track", "nowhere.test/acme"]).code, 2);
}

#[test]
fn only_one_sync_runs_at_a_time() {
    let env = Env::new();
    env.make_repo("acme", "app");
    env.ok(&["track", "local.test/acme"]);

    let lock = std::fs::File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(env.p("home").join("sync.lock"))
        .unwrap();
    lock.lock().unwrap();
    let blocked = env.weir(&["sync"]);
    assert_eq!(blocked.code, 2);
    assert!(
        blocked.stderr.contains("already running"),
        "{}",
        blocked.stderr
    );
    assert!(!env.mirror("acme", "app").exists());

    drop(lock);
    env.ok(&["sync"]);
    env.assert_mirror("acme", "app");
}

// ------------------------------------------------- a stand-in for GitHub

type Reply = (u16, Option<String>, String);

/// Serves a tiny imitation of the GitHub API on a local port.
fn serve(listener: TcpListener, route: impl Fn(&str, &str) -> Reply + Send + 'static) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut raw = Vec::new();
            let mut buf = [0u8; 2048];
            while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => raw.extend_from_slice(&buf[..n]),
                }
            }
            let request = String::from_utf8_lossy(&raw).into_owned();
            let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
            let token = request
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("authorization")
                        .then(|| v.trim().trim_start_matches("Bearer ").to_string())
                })
                .unwrap_or_default();
            let (code, link, body) = route(&path, &token);
            let link = link.map(|l| format!("Link: {l}\r\n")).unwrap_or_default();
            let _ = write!(
                stream,
                "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\n{link}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
}

fn gh_repo(id: u64, owner: &str, name: &str, clone_url: &Path, pushed_at: &str) -> String {
    format!(
        r#"{{"id":{id},"name":"{name}","owner":{{"login":"{owner}"}},"default_branch":"main","clone_url":"{}","pushed_at":"{pushed_at}","fork":false,"archived":false}}"#,
        clone_url.display()
    )
}

#[test]
fn github_accounts_paging_and_credentials() {
    let env = Env::new();
    let a = env.make_repo("acme", "a");
    let b = env.make_repo("acme", "b");
    let dotfiles = env.make_repo("homer", "dotfiles");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let next = format!("<{base}/orgs/acme/repos?type=all&per_page=100&page=2>; rel=\"next\"");
    serve(listener, move |path, token| {
        let unauthorized = (401, None, r#"{"message":"Bad credentials"}"#.to_string());
        match (path, token) {
            ("/user", "tok-work") => (200, None, r#"{"login":"worker","id":1}"#.into()),
            ("/user", "tok-home") => (200, None, r#"{"login":"homer","id":2}"#.into()),
            ("/users/acme", "tok-work") => (
                200,
                None,
                r#"{"login":"acme","id":9,"type":"Organization"}"#.into(),
            ),
            ("/orgs/acme/repos?type=all&per_page=100", "tok-work") => (
                200,
                Some(next.clone()),
                format!("[{}]", gh_repo(1, "acme", "a", &a, "t1")),
            ),
            ("/orgs/acme/repos?type=all&per_page=100&page=2", "tok-work") => (
                200,
                None,
                format!("[{}]", gh_repo(2, "acme", "b", &b, "t1")),
            ),
            ("/users/homer", "tok-home") => (
                200,
                None,
                r#"{"login":"homer","id":2,"type":"User"}"#.into(),
            ),
            ("/user/repos?affiliation=owner&per_page=100", "tok-home") => (
                200,
                None,
                format!("[{}]", gh_repo(3, "homer", "dotfiles", &dotfiles, "t1")),
            ),
            _ => unauthorized,
        }
    });

    let tokens = [("WORK_TOKEN", "tok-work"), ("HOME_TOKEN", "tok-home")];
    let weir = |args: &[&str]| env.weir_env(&tokens, args);
    assert_eq!(
        weir(&["provider", "add", "github", "ghe.test", "--api-url", &base]).code,
        0
    );

    assert_eq!(
        weir(&["track", "ghe.test/acme"]).code,
        2,
        "tracking needs an account"
    );
    let login = weir(&[
        "auth",
        "login",
        "ghe.test",
        "--env",
        "WORK_TOKEN",
        "--name",
        "work",
    ]);
    assert_eq!(login.code, 0, "{}", login.stderr);
    assert!(login.stdout.contains("as worker"), "{}", login.stdout);
    assert_eq!(weir(&["track", "ghe.test/acme"]).code, 0);

    // A second account on the same host: tracking must now say which one.
    assert_eq!(
        weir(&["auth", "login", "ghe.test", "--env", "HOME_TOKEN"]).code,
        0
    );
    let unclear = weir(&["track", "ghe.test/homer"]);
    assert_eq!(unclear.code, 2);
    assert!(
        unclear.stderr.contains("several accounts"),
        "{}",
        unclear.stderr
    );
    assert_eq!(
        weir(&["track", "ghe.test/homer", "--account", "homer"]).code,
        0
    );
    let listed = weir(&["auth", "list"]);
    assert_eq!(listed.code, 0, "{}{}", listed.stdout, listed.stderr);

    let run = weir(&["sync"]);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("3 cloned"), "{}", run.stdout);
    let root = env.p("root").join("ghe.test");
    for (dir, account) in [
        ("acme/a", "work"),
        ("acme/b", "work"),
        ("homer/dotfiles", "homer"),
    ] {
        let helpers = env.git(
            &root.join(dir),
            &["config", "--local", "--get-all", "credential.helper"],
        );
        assert_eq!(
            helpers.lines().last().unwrap(),
            format!("!weir credential --account {account}"),
            "{dir}"
        );
    }

    let cred = weir(&["credential", "--account", "work", "get"]);
    assert_eq!(cred.stdout, "username=worker\npassword=tok-work\n");
    assert_eq!(
        weir(&["credential", "--account", "work", "store"]).stdout,
        ""
    );

    // An expired token is reported once, and the other account still syncs.
    let expired = env.weir_env(
        &[("WORK_TOKEN", "expired"), ("HOME_TOKEN", "tok-home")],
        &["sync", "--all"],
    );
    assert_eq!(expired.code, 1, "{}{}", expired.stdout, expired.stderr);
    assert_eq!(
        expired.stderr.matches("account work").count(),
        1,
        "{}",
        expired.stderr
    );
    assert!(expired.stdout.contains("1 unchanged"), "{}", expired.stdout);
    // A failed listing must never mark mirrors as gone.
    assert!(!expired.stdout.contains("gone"), "{}", expired.stdout);
    assert_eq!(weir(&["list"]).stdout.lines().count(), 3);

    assert_eq!(
        weir(&["auth", "logout", "work"]).code,
        2,
        "still tracking acme"
    );
}
