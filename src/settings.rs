//! The few settings weir has. They live in the database's `config` table.

use anyhow::{bail, Result};
use rusqlite::Connection;
use std::path::PathBuf;

use crate::{db, util};

pub struct Setting {
    pub key: &'static str,
    pub default: &'static str,
    pub help: &'static str,
}

pub const SETTINGS: [Setting; 6] = [
    Setting {
        key: "root",
        default: "~/Repos",
        help: "Directory the mirrors live in",
    },
    Setting {
        key: "stale_after_hours",
        default: "36",
        help: "`weir status` warns, and exits 1, when no sync has succeeded for this long (0 turns it off)",
    },
    Setting {
        key: "on_problem",
        default: "",
        help: "Shell command to run when a sync ends with problems; it receives the summary on stdin and in $WEIR_SUMMARY",
    },
    Setting {
        key: "interval_seconds",
        default: "300",
        help: "How often `weir daemon` checks the providers",
    },
    Setting {
        key: "daemon_jobs",
        default: "4",
        help: "How many repositories `weir daemon` works on at once",
    },
    Setting {
        key: "pause_on_battery",
        default: "false",
        help: "Whether `weir daemon` waits while the machine runs on its battery (true or false)",
    },
];

fn find(key: &str) -> Result<&'static Setting> {
    match SETTINGS.iter().find(|s| s.key == key) {
        Some(s) => Ok(s),
        None => {
            let known: Vec<&str> = SETTINGS.iter().map(|s| s.key).collect();
            bail!(
                "unknown setting {key:?}; the settings are: {}",
                known.join(", ")
            )
        }
    }
}

/// The value of a setting, or its default.
pub fn get(conn: &Connection, key: &str) -> Result<String> {
    let setting = find(key)?;
    Ok(db::config_get(conn, key)?.unwrap_or_else(|| setting.default.to_string()))
}

pub fn set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    find(key)?;
    match key {
        "root" if value.trim().is_empty() => bail!("root cannot be empty"),
        "stale_after_hours" | "interval_seconds" | "daemon_jobs"
            if value.parse::<u64>().is_err() =>
        {
            bail!("{key} must be a whole number")
        }
        "interval_seconds" | "daemon_jobs" if value == "0" => bail!("{key} must be at least 1"),
        "pause_on_battery" if !matches!(value, "true" | "false") => {
            bail!("{key} must be true or false")
        }
        _ => {}
    }
    db::config_set(conn, key, value)
}

pub fn unset(conn: &Connection, key: &str) -> Result<()> {
    find(key)?;
    conn.execute("DELETE FROM config WHERE key = ?1", [key])?;
    Ok(())
}

/// Where mirrors are kept.
pub fn root(conn: &Connection) -> Result<PathBuf> {
    Ok(util::expand_tilde(&get(conn, "root")?))
}

pub fn number(conn: &Connection, key: &str) -> Result<u64> {
    let raw = get(conn, key)?;
    match raw.parse() {
        Ok(n) => Ok(n),
        Err(_) => bail!("setting {key} is {raw:?}, which is not a whole number"),
    }
}
