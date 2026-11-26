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

    /// Clones an upstream repository to where its mirror would be, the way a
    /// clone made before weir existed would look.
    fn clone_into(&self, upstream: &Path, owner: &str, name: &str) -> PathBuf {
        let dest = self.mirror(owner, name);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        self.git(
            dest.parent().unwrap(),
            &["clone", "-q", upstream.to_str().unwrap(), name],
        );
        dest
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
    // A repository named after its owner does not make the owner ambiguous.
    env.make_repo("other", "other");
    env.ok(&["sync", "other"]);
    assert_eq!(env.ok(&["list", "other"]).stdout.lines().count(), 2);

    // Tracking the whole owner takes over from the single repository.
    assert_eq!(env.weir(&["track", "local.test/other/app"]).code, 2);
    env.ok(&["track", "local.test/acme"]);
    env.ok(&["sync"]);
    assert!(env.mirror("acme", "lib").exists());

    let untracked = env.ok(&["untrack", "local.test/other"]);
    assert!(
        untracked.stdout.contains("2 mirror(s) stay on disk"),
        "{}",
        untracked.stdout
    );
    assert!(env.mirror("other", "app").exists());
    assert!(!env.ok(&["list"]).stdout.contains("other"));
    assert_eq!(env.weir(&["track", "nowhere.test/acme"]).code, 2);
}

#[test]
fn adopts_a_clone_whose_remote_uses_an_old_name() {
    let env = Env::new();
    let bare = env.make_repo("acme", "new");
    std::fs::write(bare.join("weir-old-names"), "acme/old\n").unwrap();
    let old_url = env.upstream("acme", "old");

    // The directory already has the new name; only its remote is out of date.
    let m = env.clone_into(&bare, "acme", "new");
    env.git(
        &m,
        &["remote", "set-url", "origin", old_url.to_str().unwrap()],
    );

    env.ok(&["track", "local.test/acme"]);
    let run = env.ok(&["sync"]);
    assert!(
        run.stdout.contains("adopted   local.test/acme/new"),
        "{}",
        run.stdout
    );
    assert!(env
        .git(&m, &["config", "--get", "remote.origin.url"])
        .ends_with("new.git"));
    env.assert_mirror("acme", "new");
}

#[test]
fn moves_a_clone_left_under_an_old_name_instead_of_cloning() {
    let env = Env::new();
    let bare = env.make_repo("acme", "new");
    std::fs::write(bare.join("weir-old-names"), "acme/old\n").unwrap();
    let old = env.clone_into(&bare, "acme", "old");
    env.git(
        &old,
        &[
            "remote",
            "set-url",
            "origin",
            env.upstream("acme", "old").to_str().unwrap(),
        ],
    );
    std::fs::write(old.join("untracked.txt"), "keep me\n").unwrap();

    env.ok(&["track", "local.test/acme"]);
    let run = env.ok(&["sync"]);
    assert!(!run.stdout.contains("cloned"), "{}", run.stdout);
    assert!(run.stdout.contains("moved from"), "{}", run.stdout);
    assert!(!old.exists());
    // The file that only existed in the old clone proves it was moved, not cloned.
    let marker = env.mirror("acme", "new").join("untracked.txt");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me\n");
    std::fs::remove_file(&marker).unwrap();
    env.assert_mirror("acme", "new");
}

#[test]
fn keeps_mirrors_of_archived_repositories() {
    let env = Env::new();
    env.make_repo("acme", "app");
    let lib = env.make_repo("acme", "lib");
    // Archived before weir ever ran, but a clone of it is already on disk.
    let relic = env.make_repo("acme", "relic");
    std::fs::write(relic.join("weir-archived"), "").unwrap();
    env.clone_into(&relic, "acme", "relic");
    // Archived and never cloned: it stays that way.
    let never = env.make_repo("acme", "never");
    std::fs::write(never.join("weir-archived"), "").unwrap();

    env.ok(&["track", "local.test/acme"]);
    let first = env.ok(&["sync"]);
    assert!(
        first.stdout.contains("adopted   local.test/acme/relic"),
        "{}",
        first.stdout
    );
    assert!(!env.mirror("acme", "never").exists());

    env.push("acme", "lib", "README.md", "last words\n");
    std::fs::write(lib.join("weir-archived"), "").unwrap();
    let run = env.ok(&["sync"]);
    assert!(
        run.stdout.contains("archived  local.test/acme/lib"),
        "{}",
        run.stdout
    );
    // The final push before archiving still lands in the mirror.
    env.assert_mirror("acme", "lib");
    assert_eq!(env.ok(&["list"]).stdout.trim(), "local.test/acme/app");
    let with_archived = env.ok(&["list", "--archived"]).stdout;
    assert!(
        with_archived.contains("local.test/acme/lib  (archived)"),
        "{with_archived}"
    );
    assert!(
        with_archived.contains("local.test/acme/relic  (archived)"),
        "{with_archived}"
    );
    assert_eq!(
        PathBuf::from(env.ok(&["path", "acme/lib"]).stdout.trim()),
        env.mirror("acme", "lib")
    );
    assert!(env.ok(&["status"]).stdout.contains("(+2 archived)"));

    // Later syncs leave it alone, and prune does not take it unless asked.
    let quiet = env.ok(&["sync"]);
    assert!(quiet.stdout.contains("3 unchanged"), "{}", quiet.stdout);
    assert!(env
        .ok(&["prune", "--yes"])
        .stdout
        .contains("Nothing to prune"));
    assert!(env.mirror("acme", "lib").exists());

    // Unarchived upstream: it is an ordinary mirror again.
    std::fs::remove_file(lib.join("weir-archived")).unwrap();
    env.ok(&["sync"]);
    assert!(env.ok(&["list"]).stdout.contains("local.test/acme/lib"));

    let pruned = env.ok(&["prune", "--archived", "--yes"]);
    assert!(
        pruned.stdout.contains("Deleted 1 mirror"),
        "{}",
        pruned.stdout
    );
    assert!(!env.mirror("acme", "relic").exists());
    let after = env.ok(&["sync"]);
    assert!(!after.stdout.contains("cloned"), "{}", after.stdout);
}

#[test]
fn tells_gone_from_inaccessible_from_transferred() {
    let env = Env::new();
    let hidden = env.make_repo("acme", "hidden");
    let moved = env.make_repo("acme", "moved");
    std::fs::write(moved.join("weir-id"), "77\n").unwrap();
    let deleted = env.make_repo("acme", "deleted");
    env.make_repo("acme", "app");
    env.ok(&["track", "local.test/acme"]);
    env.ok(&["sync"]);

    std::fs::write(hidden.join("weir-forbidden"), "").unwrap();
    std::fs::create_dir_all(env.p("up").join("elsewhere")).unwrap();
    std::fs::rename(&moved, env.upstream("elsewhere", "moved")).unwrap();
    std::fs::remove_dir_all(&deleted).unwrap();

    let run = env.ok(&["sync"]);
    assert!(
        run.stdout.contains("inaccessible local.test/acme/hidden"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("transferred local.test/acme/moved"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("now elsewhere/moved"), "{}", run.stdout);
    assert!(
        run.stdout.contains("gone      local.test/acme/deleted"),
        "{}",
        run.stdout
    );
    assert_eq!(env.ok(&["list"]).stdout.trim(), "local.test/acme/app");

    // A mirror the account can no longer read is never pruned.
    let pruned = env.ok(&["prune", "--yes"]);
    assert!(
        pruned.stdout.contains("Deleted 2 mirror"),
        "{}",
        pruned.stdout
    );
    assert!(env.mirror("acme", "hidden").exists());
    assert!(!env.mirror("acme", "moved").exists());
    assert!(!env.mirror("acme", "deleted").exists());

    // Access comes back: it is an ordinary mirror again.
    std::fs::remove_file(hidden.join("weir-forbidden")).unwrap();
    env.ok(&["sync"]);
    assert!(env.ok(&["list"]).stdout.contains("local.test/acme/hidden"));
}

#[test]
fn follows_a_transfer_to_another_tracked_owner() {
    let env = Env::new();
    let bare = env.make_repo("acme", "app");
    std::fs::write(bare.join("weir-id"), "5\n").unwrap();
    env.make_repo("zeta", "other");
    env.ok(&["track", "local.test/acme"]);
    env.ok(&["track", "local.test/zeta"]);
    env.ok(&["sync"]);
    std::fs::write(env.mirror("acme", "app").join("untracked.txt"), "keep me\n").unwrap();

    std::fs::rename(&bare, env.upstream("zeta", "app")).unwrap();
    let run = env.ok(&["sync"]);
    assert!(
        run.stdout.contains("moved     local.test/zeta/app"),
        "{}",
        run.stdout
    );
    assert!(!run.stdout.contains("gone"), "{}", run.stdout);
    assert!(!env.mirror("acme", "app").exists());
    assert!(env.mirror("zeta", "app").join("untracked.txt").exists());
}

#[test]
fn follows_a_repository_that_was_deleted_and_created_again() {
    let env = Env::new();
    let bare = env.make_repo("acme", "app");
    std::fs::write(bare.join("weir-id"), "1\n").unwrap();
    env.ok(&["track", "local.test/acme"]);
    env.ok(&["sync"]);

    std::fs::remove_dir_all(&bare).unwrap();
    std::fs::remove_dir_all(env.work("acme", "app")).unwrap();
    env.make_repo("acme", "app");
    env.push("acme", "app", "fresh.txt", "a different history\n");
    std::fs::write(bare.join("weir-id"), "2\n").unwrap();

    let run = env.ok(&["sync"]);
    assert!(
        run.stdout.contains("recreated local.test/acme/app"),
        "{}",
        run.stdout
    );
    assert!(!run.stdout.contains("gone"), "{}", run.stdout);
    env.assert_mirror("acme", "app");
    assert!(env.mirror("acme", "app").join("fresh.txt").exists());
    assert_eq!(
        env.ok(&["list", "--all"]).stdout.trim(),
        "local.test/acme/app"
    );
}

#[test]
fn follows_a_new_default_branch_and_a_force_push() {
    let env = Env::new();
    let bare = env.make_repo("acme", "app");
    env.ok(&["track", "local.test/acme"]);
    env.ok(&["sync"]);

    // The default branch is rewritten, not just extended.
    let work = env.work("acme", "app");
    std::fs::write(work.join("README.md"), "rewritten\n").unwrap();
    env.git(&work, &["commit", "-q", "-a", "--amend", "-m", "rewritten"]);
    env.git(&work, &["push", "-q", "-f", "origin", "HEAD:main"]);
    env.ok(&["sync"]);
    env.assert_mirror("acme", "app");

    // The provider switches the default branch to develop.
    env.git(&work, &["checkout", "-q", "-b", "develop"]);
    std::fs::write(work.join("dev.txt"), "on develop\n").unwrap();
    env.git(&work, &["add", "."]);
    env.git(&work, &["commit", "-q", "-m", "develop"]);
    env.git(&work, &["push", "-q", "origin", "develop"]);
    env.git(&bare, &["symbolic-ref", "HEAD", "refs/heads/develop"]);
    let run = env.ok(&["sync"]);
    assert!(run.stdout.contains("updated"), "{}", run.stdout);
    let m = env.mirror("acme", "app");
    assert_eq!(
        env.git(&m, &["rev-parse", "HEAD"]),
        env.git(&bare, &["rev-parse", "develop"])
    );
    assert!(m.join("dev.txt").exists());
    assert!(env.try_git(&m, &["symbolic-ref", "-q", "HEAD"]).is_none());
}

#[test]
fn doctor_explains_unmanaged_clones_and_prune_removes_only_duplicates() {
    let env = Env::new();
    let app = env.make_repo("acme", "app");
    std::fs::write(app.join("weir-old-names"), "acme/app-old\n").unwrap();
    let vanished = env.make_repo("acme", "vanished");
    let side = env.make_repo("other", "side");
    env.ok(&["track", "local.test/acme"]);
    env.ok(&["sync"]);
    assert!(env.ok(&["doctor"]).stdout.contains("is managed by weir"));

    // A stale copy of app under its old name.
    let copy = env.clone_into(&app, "acme", "app-old");
    env.git(
        &copy,
        &[
            "remote",
            "set-url",
            "origin",
            env.upstream("acme", "app-old").to_str().unwrap(),
        ],
    );
    // A repository that only exists here.
    let mystery = env.mirror("acme", "mystery");
    std::fs::create_dir_all(&mystery).unwrap();
    env.git(&mystery, &["init", "-q"]);
    // A clone of a repository nobody tracks.
    env.clone_into(&side, "other", "side");
    // A clone whose repository was deleted upstream and pruned from weir.
    std::fs::remove_dir_all(&vanished).unwrap();
    env.ok(&["sync"]);
    env.ok(&["prune", "--yes"]);
    std::fs::remove_dir_all(env.work("acme", "vanished")).unwrap();
    let again = env.make_repo("acme", "vanished");
    env.clone_into(&again, "acme", "vanished-copy");
    std::fs::remove_dir_all(&again).unwrap();

    let doctor = env.ok(&["doctor"]).stdout;
    for expected in [
        "4 git repositories",
        "duplicate     ",
        "a stale copy of local.test/acme/app",
        "no-remote     ",
        "untracked     ",
        "is local.test/other/side, which is not tracked",
        "gone          ",
    ] {
        assert!(
            doctor.contains(expected),
            "missing {expected:?} in:\n{doctor}"
        );
    }
    assert!(env
        .ok(&["status"])
        .stdout
        .contains("4 git repositories under the root are not managed"));

    let pruned = env.ok(&["prune", "--unmanaged", "--yes"]);
    assert!(
        pruned.stdout.contains("Deleted 1 mirror"),
        "{}",
        pruned.stdout
    );
    assert!(!copy.exists());
    assert!(mystery.exists());
    assert!(env.mirror("other", "side").exists());
    assert!(env.mirror("acme", "vanished-copy").exists());
    env.assert_mirror("acme", "app");
}

fn db(env: &Env) -> rusqlite::Connection {
    rusqlite::Connection::open(env.p("home").join("weir.db")).unwrap()
}

#[test]
fn status_warns_when_syncs_stop_succeeding() {
    let env = Env::new();
    env.make_repo("acme", "app");
    env.ok(&["track", "local.test/acme"]);
    env.ok(&["sync"]);
    env.ok(&["sync"]);
    let fresh = env.ok(&["status"]);
    assert!(
        fresh.stdout.contains("Last 2 runs: 2 ok, 0 with problems"),
        "{}",
        fresh.stdout
    );
    assert!(!fresh.stdout.contains("WARNING"), "{}", fresh.stdout);

    // Pretend the last success was long ago.
    db(&env)
        .execute(
            "UPDATE sync_runs SET started_at = '2020-01-01T08:30:00Z', finished_at = '2020-01-01T08:31:00Z'",
            [],
        )
        .unwrap();
    let stale = env.weir(&["status"]);
    assert_eq!(stale.code, 1, "{}", stale.stdout);
    assert!(stale
        .stdout
        .contains("WARNING: no sync has succeeded since 2020-01-01T08:31:00Z"));
    let json: serde_json::Value =
        serde_json::from_str(&env.weir(&["status", "--json"]).stdout).unwrap();
    assert_eq!(json["health"]["stale"], true);

    // The limit is a setting, and 0 turns the check off.
    env.ok(&["config", "set", "stale_after_hours", "0"]);
    assert_eq!(env.weir(&["status"]).code, 0);
    env.ok(&["config", "unset", "stale_after_hours"]);
    assert_eq!(env.weir(&["status"]).code, 1);
    assert!(env
        .ok(&["config", "list"])
        .stdout
        .contains("stale_after_hours  36"));
    assert_eq!(env.weir(&["config", "set", "nope", "1"]).code, 2);
    assert_eq!(
        env.weir(&["config", "set", "stale_after_hours", "soon"])
            .code,
        2
    );

    // A run that died is reported as interrupted, then recorded as such.
    env.ok(&["sync"]);
    db(&env)
        .execute(
            "INSERT INTO sync_runs (started_at, phase, pid) VALUES ('2026-01-01T00:00:00Z', 'syncing', 1)",
            [],
        )
        .unwrap();
    let dead = env.ok(&["status"]);
    assert!(
        dead.stdout.contains("was interrupted before it finished"),
        "{}",
        dead.stdout
    );
    assert!(!dead.stdout.contains("Sync running"), "{}", dead.stdout);
    env.ok(&["sync"]);
    assert!(env.ok(&["log"]).stdout.contains("interrupted"));
}

#[test]
fn sync_streams_json_and_records_timings() {
    let env = Env::new();
    env.make_repo("acme", "app");
    env.make_repo("acme", "lib");
    env.ok(&["track", "local.test/acme"]);

    let run = env.ok(&["sync", "--json"]);
    let events: Vec<serde_json::Value> = run
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON: {l}: {e}")))
        .collect();
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["start", "plan", "repo", "repo", "finish"]);
    assert_eq!(events[1]["jobs"], 2);
    assert_eq!(events[2]["action"], "cloned");
    assert!(events[2]["millis"].as_i64().unwrap() >= 0);
    assert_eq!(events[4]["ok"], true);
    assert_eq!(events[4]["counts"]["cloned"], 2);

    // Repositories that were fetched and had not changed are recorded too.
    env.ok(&["sync", "--all"]);
    assert!(!env
        .ok(&["log", "-n", "1"])
        .stdout
        .contains("unchanged local.test/acme/app"));
    assert!(env
        .ok(&["log", "-n", "1", "--all"])
        .stdout
        .contains("unchanged local.test/acme/app"));
    let slow = env.ok(&["log", "--slow"]).stdout;
    assert!(slow.contains("s  cloned    local.test/acme/"), "{slow}");

    // The log file keeps a readable trail of every run.
    let trail = std::fs::read_to_string(env.p("home").join("weir.log")).unwrap();
    assert!(
        trail.contains("run 1  cloned    local.test/acme/app"),
        "{trail}"
    );
    assert!(trail.contains("run 2  finished: 2 unchanged"), "{trail}");
}

#[test]
fn runs_a_command_when_a_sync_has_problems() {
    let env = Env::new();
    let app = env.make_repo("acme", "app");
    env.make_repo("acme", "other");
    env.ok(&["track", "local.test/acme"]);
    let note = env.p("note.txt");
    let summary = env.p("summary.txt");
    env.ok(&[
        "config",
        "set",
        "on_problem",
        &format!(
            "cat > {}; printf %s \"$WEIR_SUMMARY\" > {}",
            note.display(),
            summary.display()
        ),
    ]);

    env.ok(&["sync"]);
    assert!(!note.exists(), "a clean sync must not run the command");

    // Make one mirror impossible to update.
    let file = env.mirror("acme", "app").join("README.md");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(&file, "my edit\n").unwrap();
    env.push("acme", "app", "README.md", "two\n");
    let _ = app;
    assert_eq!(env.weir(&["sync"]).code, 1);
    let told = std::fs::read_to_string(&note).unwrap();
    assert!(told.contains("skipped   local.test/acme/app"), "{told}");
    assert_eq!(
        std::fs::read_to_string(&summary).unwrap(),
        "1 skipped, 1 unchanged"
    );
}

#[test]
fn skips_repositories_over_the_size_limit() {
    let env = Env::new();
    env.make_repo("acme", "app");
    let big = env.make_repo("acme", "big");
    std::fs::write(big.join("weir-size"), "2048000\n").unwrap();
    assert_eq!(
        env.weir(&["track", "local.test/acme", "--max-size", "0"])
            .code,
        2
    );
    env.ok(&["track", "local.test/acme", "--max-size", "500"]);
    env.ok(&["sync"]);
    assert!(!env.mirror("acme", "big").exists());
    assert_eq!(env.ok(&["list"]).stdout.trim(), "local.test/acme/app");
    assert!(env.ok(&["status"]).stdout.contains("up to 500 MB each"));
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
    let blocked = env.weir(&["sync", "--no-wait"]);
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

#[test]
fn status_and_watch_follow_a_sync_in_progress() {
    let env = Env::new();
    let a = env.make_repo("acme", "a");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    serve(listener, move |path, token| match (path, token) {
        ("/user", "tok") => (200, None, r#"{"login":"worker","id":1}"#.into()),
        ("/users/acme", "tok") => (
            200,
            None,
            r#"{"login":"acme","id":9,"type":"Organization"}"#.into(),
        ),
        ("/orgs/acme/repos?type=all&per_page=100", "tok") => (
            200,
            None,
            format!("[{}]", gh_repo(1, "acme", "a", &a, "t1")),
        ),
        _ => (401, None, r#"{"message":"Bad credentials"}"#.to_string()),
    });
    env.ok(&["provider", "add", "github", "ghe.test", "--api-url", &base]);
    // A token that takes three seconds to fetch keeps the sync in its listing phase.
    env.ok(&[
        "auth",
        "login",
        "ghe.test",
        "--name",
        "work",
        "--command",
        "sleep 3; echo tok",
    ]);
    env.ok(&["track", "ghe.test/acme"]);

    std::thread::scope(|s| {
        let sync = s.spawn(|| env.weir(&["sync"]));
        std::thread::sleep(std::time::Duration::from_millis(1200));

        let status = env.weir(&["status"]);
        assert!(
            status.stdout.contains("Sync running: listing repositories"),
            "{}",
            status.stdout
        );
        let second = env.weir(&["sync", "--no-wait"]);
        assert_eq!(second.code, 2, "{}", second.stderr);

        // Attached from "another terminal": it sees the rest of the run and its result.
        let watched = env.weir(&["watch"]);
        assert_eq!(watched.code, 0, "{}{}", watched.stdout, watched.stderr);
        assert!(
            watched.stdout.contains("cloned    ghe.test/acme/a"),
            "{}",
            watched.stdout
        );
        assert!(
            watched.stdout.contains("weir: 1 cloned in"),
            "{}",
            watched.stdout
        );

        let done = sync.join().unwrap();
        assert_eq!(done.code, 0, "{}{}", done.stdout, done.stderr);
    });
    assert!(env.ok(&["watch"]).stdout.contains("No sync is running."));
    assert!(!env.ok(&["status"]).stdout.contains("Sync running"));
}

fn wait_until(what: &str, done: &dyn Fn() -> bool) {
    for _ in 0..150 {
        if done() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn daemon_keeps_mirrors_current_until_stopped() {
    let env = Env::new();
    env.make_repo("acme", "app");
    env.ok(&["track", "local.test/acme"]);

    let mut c = Command::new(env!("CARGO_BIN_EXE_weir"));
    env.isolate(&mut c);
    let child = c
        .args(["daemon", "--interval", "1"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // It clones what is tracked, without anyone running `weir sync`.
    let app = env.mirror("acme", "app");
    wait_until("the first clone", &|| app.join(".git").is_dir());
    // A push shows up within an interval.
    env.push("acme", "app", "README.md", "two\n");
    let want = env.upstream_head("acme", "app");
    wait_until("the push to arrive", &|| {
        env.try_git(&app, &["rev-parse", "HEAD"]).as_deref() == Some(want.as_str())
    });
    // So does a new repository.
    env.make_repo("acme", "new");
    wait_until("the new repository", &|| {
        env.mirror("acme", "new").join(".git").is_dir()
    });

    assert!(env.ok(&["status"]).stdout.contains("Daemon running"));
    let second = env.weir(&["daemon"]);
    assert_eq!(second.code, 2, "a second daemon must be refused");
    assert!(
        second.stderr.contains("already running"),
        "{}",
        second.stderr
    );
    // A manual sync works alongside it: it waits its turn.
    assert_eq!(env.weir(&["sync"]).code, 0);

    // Cycles with nothing to do must not pile up in the history.
    std::thread::sleep(std::time::Duration::from_millis(3500));
    let conn = db(&env);
    let idle: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_runs WHERE idle = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_runs", [], |r| r.get(0))
        .unwrap();
    assert!(idle <= 1, "{idle} idle runs kept");
    assert!(total <= 5, "{total} runs kept");

    // SIGTERM stops it cleanly.
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{:?}", out.status);
    let notes = String::from_utf8_lossy(&out.stderr);
    assert!(notes.contains("weir daemon stopped"), "{notes}");
    assert!(!env.ok(&["status"]).stdout.contains("Daemon running"));
    assert_eq!(env.weir(&["service", "status"]).code, 1);
    // A service that Homebrew registered is recognized, and weir will not
    // register a second one next to it.
    let brew_service = env.p("userhome").join(if cfg!(target_os = "macos") {
        "Library/LaunchAgents/sh.brew.weir.plist"
    } else {
        ".config/systemd/user/sh.brew.weir.service"
    });
    std::fs::create_dir_all(brew_service.parent().unwrap()).unwrap();
    std::fs::write(&brew_service, "").unwrap();
    let status = env.weir(&["service", "status"]);
    assert!(
        status.stdout.contains("Service managed by Homebrew"),
        "{}",
        status.stdout
    );
    let refused = env.weir(&["service", "install"]);
    assert_eq!(refused.code, 2);
    assert!(
        refused.stderr.contains("brew services start weir"),
        "{}",
        refused.stderr
    );
    assert!(env
        .ok(&["status"])
        .stdout
        .contains("`brew services start weir` starts it"));
    env.assert_mirror("acme", "app");
    env.assert_mirror("acme", "new");
}

#[test]
fn daemon_settings_are_validated() {
    let env = Env::new();
    env.ok(&["config", "set", "interval_seconds", "60"]);
    assert_eq!(
        env.weir(&["config", "set", "interval_seconds", "0"]).code,
        2
    );
    assert_eq!(
        env.weir(&["config", "set", "pause_on_battery", "maybe"])
            .code,
        2
    );
    env.ok(&["config", "set", "pause_on_battery", "true"]);
    let list = env.ok(&["config", "list"]).stdout;
    assert!(list.contains("interval_seconds   60"), "{list}");
    // One cycle, then exit: nothing tracked is an error it reports, not a crash.
    let once = env.weir(&["config", "set", "pause_on_battery", "false"]);
    assert_eq!(once.code, 0);
    env.make_repo("acme", "app");
    env.ok(&["track", "local.test/acme"]);
    let run = env.weir(&["daemon", "--cycles", "1"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    env.assert_mirror("acme", "app");
}

#[test]
fn finishes_its_own_interrupted_checkout() {
    let env = Env::new();
    env.make_repo("acme", "app");
    env.ok(&["track", "local.test/acme"]);
    env.ok(&["sync"]);

    // What a kill in the middle of a checkout leaves: the marker weir wrote
    // before it started, and files that match neither commit.
    env.push("acme", "app", "README.md", "two\n");
    let m = env.mirror("acme", "app");
    let file = m.join("README.md");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(&file, "half writ").unwrap();
    std::fs::write(m.join(".git").join("weir-checkout"), "interrupted").unwrap();

    let run = env.ok(&["sync"]);
    assert!(
        run.stdout
            .contains("finished a checkout that was interrupted"),
        "{}",
        run.stdout
    );
    assert!(!m.join(".git").join("weir-checkout").exists());
    env.assert_mirror("acme", "app");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "two\n");
}

#[test]
fn an_interrupted_clone_leaves_nothing_behind() {
    let env = Env::new();
    env.make_repo("acme", "app");
    // What a killed clone leaves: a temporary directory next to the mirror's path.
    let leftover = env.mirror("acme", ".weir-clone-app-99999");
    std::fs::create_dir_all(leftover.join(".git")).unwrap();
    std::fs::write(leftover.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    env.ok(&["track", "local.test/acme"]);
    // It is not mistaken for a clone someone made.
    assert!(env.ok(&["doctor"]).stdout.contains("is managed by weir"));
    let run = env.ok(&["sync"]);
    assert!(
        run.stdout.contains("cloned    local.test/acme/app"),
        "{}",
        run.stdout
    );
    assert!(!leftover.exists());
    env.assert_mirror("acme", "app");
    let names: Vec<String> = std::fs::read_dir(env.mirror("acme", "app").parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["app"]);
}

#[test]
fn a_sync_waits_for_the_one_that_is_running() {
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
    std::thread::scope(|s| {
        let waiting = s.spawn(|| env.weir(&["sync"]));
        std::thread::sleep(std::time::Duration::from_millis(1200));
        assert!(
            !env.mirror("acme", "app").exists(),
            "it must not start early"
        );
        drop(lock);
        let run = waiting.join().unwrap();
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert!(
            run.stderr.contains("waiting for it to finish"),
            "{}",
            run.stderr
        );
    });
    env.assert_mirror("acme", "app");
}

/// A stand-in for GitHub that sees the whole request, for tests about headers.
fn serve_raw(listener: TcpListener, respond: impl Fn(&str) -> String + Send + 'static) {
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
            let response = respond(&String::from_utf8_lossy(&raw));
            let _ = stream.write_all(response.as_bytes());
        }
    });
}

fn http(code: u16, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[test]
fn unchanged_listings_cost_nothing_and_the_rate_limit_is_respected() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    let env = Env::new();
    let a = env.make_repo("acme", "a");
    let b = env.make_repo("beta", "b");
    let not_modified = Arc::new(AtomicUsize::new(0));
    let beta_requests = Arc::new(AtomicUsize::new(0));
    let nearly_out = Arc::new(AtomicBool::new(false));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (seen_304, seen_beta, low) = (
        not_modified.clone(),
        beta_requests.clone(),
        nearly_out.clone(),
    );
    serve_raw(listener, move |request| {
        let path = request.split_whitespace().nth(1).unwrap_or("");
        let lower = request.to_lowercase();
        let reset = 4_000_000_000u64;
        let left = if low.load(Ordering::Relaxed) { 3 } else { 4000 };
        let limit = format!("x-ratelimit-remaining: {left}\r\nx-ratelimit-reset: {reset}\r\n");
        // Each resource has an ETag; a request that already has it gets a 304.
        let conditional = |etag: &str, body: String| {
            if lower.contains(&format!("if-none-match: {etag}")) {
                seen_304.fetch_add(1, Ordering::Relaxed);
                http(304, &limit, "")
            } else {
                http(200, &format!("ETag: {etag}\r\n{limit}"), &body)
            }
        };
        if path.contains("beta") {
            seen_beta.fetch_add(1, Ordering::Relaxed);
        }
        match path {
            "/user" => http(200, "", r#"{"login":"worker","id":1}"#),
            "/users/acme" => conditional(
                "\"u1\"",
                r#"{"login":"acme","id":9,"type":"Organization","bio":"not needed"}"#.into(),
            ),
            "/orgs/acme/repos?type=all&per_page=100" => {
                conditional("\"p1\"", format!("[{}]", gh_repo(1, "acme", "a", &a, "t1")))
            }
            "/users/beta" => conditional(
                "\"u2\"",
                r#"{"login":"beta","id":8,"type":"Organization"}"#.into(),
            ),
            "/orgs/beta/repos?type=all&per_page=100" => {
                conditional("\"p2\"", format!("[{}]", gh_repo(2, "beta", "b", &b, "t1")))
            }
            _ => http(404, "", r#"{"message":"Not Found"}"#),
        }
    });

    let weir = |args: &[&str]| env.weir_env(&[("TOKEN", "tok")], args);
    assert_eq!(
        weir(&["provider", "add", "github", "ghe.test", "--api-url", &base]).code,
        0
    );
    assert_eq!(
        weir(&["auth", "login", "ghe.test", "--env", "TOKEN", "--name", "work"]).code,
        0
    );
    assert_eq!(weir(&["track", "ghe.test/acme"]).code, 0);

    let first = weir(&["sync"]);
    assert_eq!(first.code, 0, "{}{}", first.stdout, first.stderr);
    assert_eq!(not_modified.load(Ordering::Relaxed), 0);

    // Nothing changed: both requests are answered "not modified", and the
    // listing still works from what was remembered.
    let second = weir(&["sync"]);
    assert_eq!(second.code, 0, "{}{}", second.stdout, second.stderr);
    assert!(second.stdout.contains("1 unchanged"), "{}", second.stdout);
    assert_eq!(not_modified.load(Ordering::Relaxed), 2);
    let cached: Vec<String> = {
        let conn = db(&env);
        let mut st = conn
            .prepare("SELECT body FROM http_cache ORDER BY url")
            .unwrap();
        let rows = st.query_map([], |r| r.get(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    };
    assert_eq!(cached.len(), 2);
    assert!(
        cached.iter().all(|body| !body.contains("not needed")),
        "{cached:?}"
    );

    // With the rate limit nearly used up, weir stops asking and says when it resets.
    assert_eq!(weir(&["track", "ghe.test/beta"]).code, 0);
    nearly_out.store(true, Ordering::Relaxed);
    let limited = weir(&["sync"]);
    assert_eq!(limited.code, 1, "{}{}", limited.stdout, limited.stderr);
    assert!(
        limited
            .stderr
            .contains("ghe.test/beta: not listed, the rate limit of ghe.test is nearly used up"),
        "{}",
        limited.stderr
    );
    assert_eq!(beta_requests.load(Ordering::Relaxed), 0);
    assert!(!env.p("root").join("ghe.test/beta").exists());

    // Once there is room again, the owner that was skipped is mirrored.
    nearly_out.store(false, Ordering::Relaxed);
    db(&env).execute("DELETE FROM http_cache", []).unwrap();
    let recovered = weir(&["sync"]);
    assert_eq!(
        recovered.code, 0,
        "{}{}",
        recovered.stdout, recovered.stderr
    );
    assert!(env.p("root").join("ghe.test/beta/b").exists());
}

/// The documentation's command reference is generated from the command-line
/// definitions. This fails when the committed pages are out of date.
#[test]
fn command_reference_is_up_to_date() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let committed = repo.join("docs/src/reference/commands");
    let env = Env::new();
    let fresh = env.p("reference");
    env.ok(&[
        "reference",
        fresh.to_str().unwrap(),
        "--examples",
        repo.join("docs/src/examples").to_str().unwrap(),
    ]);
    let names = |dir: &Path| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let hint = "regenerate with: cargo run -- reference docs/src/reference/commands --examples docs/src/examples";
    assert_eq!(names(&fresh), names(&committed), "{hint}");
    let summary = std::fs::read_to_string(repo.join("docs/src/SUMMARY.md")).unwrap();
    for name in names(&fresh) {
        assert_eq!(
            std::fs::read_to_string(fresh.join(&name)).unwrap(),
            std::fs::read_to_string(committed.join(&name)).unwrap(),
            "docs/src/reference/commands/{name} is out of date; {hint}"
        );
        assert!(
            summary.contains(&format!("reference/commands/{name}")),
            "docs/src/SUMMARY.md does not list reference/commands/{name}"
        );
    }
}

#[test]
fn daemon_steps_aside_when_its_binary_is_replaced() {
    let env = Env::new();
    env.make_repo("acme", "app");
    env.ok(&["track", "local.test/acme"]);

    // Run a copy of the binary, the way a service manager runs an installed one.
    let installed = env.p("bin").join("weir");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_weir"), &installed).unwrap();
    let mut c = Command::new(&installed);
    env.isolate(&mut c);
    let child = c
        .args(["daemon", "--interval", "1"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("the first clone", &|| {
        env.mirror("acme", "app").join(".git").is_dir()
    });

    // An upgrade puts a new file where the old one was.
    let staged = env.p("bin").join("weir.new");
    std::fs::copy(env!("CARGO_BIN_EXE_weir"), &staged).unwrap();
    std::fs::rename(&staged, &installed).unwrap();

    // It exits on its own, cleanly, so the service manager can start the new one.
    // Polled, so a daemon that never exits fails the test instead of hanging it.
    let mut child = child;
    let mut exited = false;
    for _ in 0..150 {
        if child.try_wait().unwrap().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if !exited {
        child.kill().unwrap();
        panic!("the daemon kept running after its binary was replaced");
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{:?}", out.status);
    let notes = String::from_utf8_lossy(&out.stderr);
    assert!(notes.contains("replaced by another version"), "{notes}");
    assert!(!env.ok(&["status"]).stdout.contains("Daemon running"));
}
