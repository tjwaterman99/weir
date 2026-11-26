//! `weir service`: registers the daemon with the operating system's service
//! manager (launchd on macOS, systemd on Linux) so it starts at login and is
//! restarted if it dies.

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{daemon, db, util};

pub const LABEL: &str = "dev.weir.daemon";

/// The path the service should run. A Homebrew install lives in a versioned
/// directory that changes on every upgrade; its stable link is used instead.
pub fn stable_exe(exe: &Path) -> PathBuf {
    let text = exe.to_string_lossy();
    if let Some((prefix, _)) = text.split_once("/Cellar/") {
        let link = Path::new(prefix).join("bin").join("weir");
        if link.exists() {
            return link;
        }
    }
    exe.to_path_buf()
}

/// PATH for the service: weir's own directory (git calls `weir` as its
/// credential helper), wherever `git` is, then the usual system directories.
fn service_path(exe: &Path) -> String {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut add = |dir: PathBuf| {
        if !dir.as_os_str().is_empty() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };
    if let Some(dir) = exe.parent() {
        add(dir.to_path_buf());
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.join("git").is_file() || dir.join("gh").is_file() {
                add(dir);
            }
        }
    }
    for dir in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        add(PathBuf::from(dir));
    }
    std::env::join_paths(dirs)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn launchd_plist(exe: &Path, path_env: &str, weir_home: Option<&str>, log: &Path) -> String {
    let home_entry = weir_home
        .map(|h| {
            format!(
                "\t\t<key>WEIR_HOME</key>\n\t\t<string>{}</string>\n",
                xml(h)
            )
        })
        .unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `weir service install`. Remove it with `weir service uninstall`. -->
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>daemon</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key>
		<string>{path}</string>
{home_entry}	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ThrottleInterval</key>
	<integer>30</integer>
	<key>ProcessType</key>
	<string>Background</string>
	<key>LowPriorityIO</key>
	<true/>
	<key>Nice</key>
	<integer>5</integer>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
        exe = xml(&exe.to_string_lossy()),
        path = xml(path_env),
        log = xml(&log.to_string_lossy()),
    )
}

pub fn systemd_unit(exe: &Path, path_env: &str, weir_home: Option<&str>) -> String {
    let home_line = weir_home
        .map(|h| format!("Environment=\"WEIR_HOME={h}\"\n"))
        .unwrap_or_default();
    format!(
        "# Written by `weir service install`. Remove it with `weir service uninstall`.
[Unit]
Description=weir: keeps local mirrors of repositories up to date
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=\"{exe}\" daemon
Environment=\"PATH={path_env}\"
{home_line}Restart=always
RestartSec=30
Nice=5
IOSchedulingClass=idle

[Install]
WantedBy=default.target
",
        exe = exe.display()
    )
}

enum Manager {
    Launchd,
    Systemd,
}

fn manager() -> Result<Manager> {
    if cfg!(target_os = "macos") {
        Ok(Manager::Launchd)
    } else if cfg!(target_os = "linux") {
        Ok(Manager::Systemd)
    } else {
        bail!("`weir service` supports macOS (launchd) and Linux (systemd); run `weir daemon` under your own service manager")
    }
}

pub fn definition_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        util::user_home()
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"))
    } else {
        util::user_home().join(".config/systemd/user/weir.service")
    }
}

/// Whether a binary at this (resolved) path was installed by Homebrew.
pub fn from_homebrew(exe: &Path) -> bool {
    exe.to_string_lossy().contains("/Cellar/weir/")
}

/// Where `brew services start weir` puts its service definition. Homebrew
/// has used two naming schemes; the one that exists wins, the newer otherwise.
pub fn homebrew_definition_path() -> PathBuf {
    let candidates: [&str; 2] = if cfg!(target_os = "macos") {
        [
            "Library/LaunchAgents/sh.brew.weir.plist",
            "Library/LaunchAgents/homebrew.mxcl.weir.plist",
        ]
    } else {
        [
            ".config/systemd/user/sh.brew.weir.service",
            ".config/systemd/user/homebrew.weir.service",
        ]
    };
    let home = util::user_home();
    candidates
        .iter()
        .map(|c| home.join(c))
        .find(|p| p.exists())
        .unwrap_or_else(|| home.join(candidates[0]))
}

/// The service definition that is installed, if any, and whether it is Homebrew's.
pub fn installed() -> Option<(PathBuf, bool)> {
    let brew = homebrew_definition_path();
    let own = definition_path();
    if brew.exists() {
        Some((brew, true))
    } else if own.exists() {
        Some((own, false))
    } else {
        None
    }
}

/// How to start the daemon on this machine, for messages.
pub fn start_hint() -> &'static str {
    if this_is_homebrew() {
        "brew services start weir"
    } else {
        "weir service install"
    }
}

fn this_is_homebrew() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| std::fs::canonicalize(exe).ok())
        .is_some_and(|exe| from_homebrew(&exe))
}

/// Homebrew has its own service manager, and one daemon needs one owner: two
/// registered services would fight over the daemon's lock forever.
fn leave_it_to_homebrew(action: &str) -> Result<()> {
    if this_is_homebrew() {
        bail!("this weir was installed with Homebrew, which manages its service: run `brew services {action} weir`");
    }
    if homebrew_definition_path().exists() {
        bail!("Homebrew already manages a weir service on this machine: run `brew services {action} weir`");
    }
    Ok(())
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let out = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("could not run {program}"))?;
    if !out.status.success() {
        bail!(
            "`{program} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn launchd_domain() -> Result<String> {
    let out = Command::new("id")
        .arg("-u")
        .output()
        .context("could not run id")?;
    Ok(format!(
        "gui/{}",
        String::from_utf8_lossy(&out.stdout).trim()
    ))
}

fn wait_until(running: bool) -> bool {
    for _ in 0..40 {
        if daemon::is_running() == running {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    false
}

pub fn install() -> Result<()> {
    leave_it_to_homebrew("start")?;
    let manager = manager()?;
    let exe = stable_exe(&std::env::current_exe().context("could not find the weir binary")?);
    let path_env = service_path(&exe);
    let weir_home = std::env::var("WEIR_HOME").ok().filter(|h| !h.is_empty());
    let file = definition_path();
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::create_dir_all(db::home())?;
    match manager {
        Manager::Launchd => {
            let log = db::home().join("daemon.log");
            std::fs::write(
                &file,
                launchd_plist(&exe, &path_env, weir_home.as_deref(), &log),
            )?;
            let domain = launchd_domain()?;
            // Replace a copy that is already loaded.
            let _ = run("launchctl", &["bootout", &format!("{domain}/{LABEL}")]);
            wait_until(false);
            run(
                "launchctl",
                &["bootstrap", &domain, &file.to_string_lossy()],
            )?;
        }
        Manager::Systemd => {
            std::fs::write(&file, systemd_unit(&exe, &path_env, weir_home.as_deref()))?;
            run("systemctl", &["--user", "daemon-reload"])?;
            run("systemctl", &["--user", "enable", "--now", "weir.service"])?;
        }
    }
    println!("Installed {}.", file.display());
    if wait_until(true) {
        println!("The weir daemon is running and will start again at every login.");
    } else {
        println!("The service is registered, but the daemon has not started yet; see `weir service status`.");
    }
    if matches!(manager, Manager::Systemd) {
        println!("To keep it running while you are logged out: loginctl enable-linger");
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let file = definition_path();
    match manager()? {
        Manager::Launchd => {
            let _ = run(
                "launchctl",
                &["bootout", &format!("{}/{LABEL}", launchd_domain()?)],
            );
        }
        Manager::Systemd => {
            let _ = run("systemctl", &["--user", "disable", "--now", "weir.service"]);
        }
    }
    // The service manager stops the daemon in its own time; wait for it.
    wait_until(false);
    if file.exists() {
        std::fs::remove_file(&file)?;
        if matches!(manager()?, Manager::Systemd) {
            let _ = run("systemctl", &["--user", "daemon-reload"]);
        }
        println!(
            "Removed {}. The daemon is stopped and will not start again.",
            file.display()
        );
    } else {
        println!("weir's own service is not installed.");
        if homebrew_definition_path().exists() {
            println!("Homebrew manages one: `brew services stop weir` stops it.");
        }
    }
    Ok(())
}

pub fn start() -> Result<()> {
    leave_it_to_homebrew("start")?;
    let file = definition_path();
    if !file.exists() {
        bail!("the weir service is not installed; run `weir service install`");
    }
    match manager()? {
        Manager::Launchd => {
            let domain = launchd_domain()?;
            if !daemon::is_running() {
                let _ = run(
                    "launchctl",
                    &["bootstrap", &domain, &file.to_string_lossy()],
                );
                let _ = run("launchctl", &["kickstart", &format!("{domain}/{LABEL}")]);
            }
        }
        Manager::Systemd => run("systemctl", &["--user", "start", "weir.service"])?,
    }
    if wait_until(true) {
        println!("The weir daemon is running.");
    } else {
        bail!("the daemon did not start; see `weir service status`");
    }
    Ok(())
}

pub fn stop() -> Result<()> {
    if !definition_path().exists() {
        leave_it_to_homebrew("stop")?;
    }
    match manager()? {
        Manager::Launchd => {
            let _ = run(
                "launchctl",
                &["bootout", &format!("{}/{LABEL}", launchd_domain()?)],
            );
        }
        Manager::Systemd => run("systemctl", &["--user", "stop", "weir.service"])?,
    }
    if wait_until(false) {
        println!("The weir daemon is stopped. It starts again at the next login, or with `weir service start`.");
    } else {
        bail!("the daemon is still running");
    }
    Ok(())
}

pub fn status(conn: &Connection) -> Result<i32> {
    match installed() {
        Some((file, true)) => println!("Service managed by Homebrew: {}", file.display()),
        Some((file, false)) => println!("Service installed: {}", file.display()),
        None => println!("Service not installed. `{}` sets it up.", start_hint()),
    }
    match daemon::describe(conn)? {
        Some(line) => {
            println!("{line}");
            Ok(0)
        }
        None => {
            println!("Daemon not running.");
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_launchd_agent() {
        let plist = launchd_plist(
            Path::new("/opt/homebrew/bin/weir"),
            "/opt/homebrew/bin:/usr/bin",
            Some("/tmp/a&b"),
            Path::new("/Users/me/.weir/daemon.log"),
        );
        assert!(plist.contains("<string>dev.weir.daemon</string>"));
        assert!(
            plist.contains("<string>/opt/homebrew/bin/weir</string>\n\t\t<string>daemon</string>")
        );
        assert!(plist.contains("<key>KeepAlive</key>\n\t<true/>"));
        assert!(plist.contains("<string>/tmp/a&amp;b</string>"));
        assert!(plist.contains("<string>/Users/me/.weir/daemon.log</string>"));
        assert!(
            !launchd_plist(Path::new("/w"), "/usr/bin", None, Path::new("/l"))
                .contains("WEIR_HOME")
        );
    }

    #[test]
    fn writes_a_systemd_unit() {
        let unit = systemd_unit(
            Path::new("/usr/local/bin/weir"),
            "/usr/local/bin:/usr/bin",
            None,
        );
        assert!(unit.contains("ExecStart=\"/usr/local/bin/weir\" daemon"));
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(!unit.contains("WEIR_HOME"));
    }

    #[test]
    fn recognizes_a_homebrew_install() {
        assert!(from_homebrew(Path::new(
            "/opt/homebrew/Cellar/weir/0.2.1/bin/weir"
        )));
        assert!(from_homebrew(Path::new(
            "/home/linuxbrew/.linuxbrew/Cellar/weir/1.0.0/bin/weir"
        )));
        assert!(!from_homebrew(Path::new("/usr/local/bin/weir")));
        assert!(!from_homebrew(Path::new("/Users/me/.cargo/bin/weir")));
    }

    #[test]
    fn leaves_ordinary_paths_alone() {
        assert_eq!(
            stable_exe(Path::new("/usr/local/bin/weir")),
            PathBuf::from("/usr/local/bin/weir")
        );
        // A versioned Homebrew path with no stable link next to it is kept.
        assert_eq!(
            stable_exe(Path::new("/nonexistent/Cellar/weir/1.0/bin/weir")),
            PathBuf::from("/nonexistent/Cellar/weir/1.0/bin/weir")
        );
    }
}
