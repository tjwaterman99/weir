//! The single SQLite database that holds everything weir knows.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::PathBuf;

use crate::util;

const SCHEMA: &str = "
CREATE TABLE providers (
  id      INTEGER PRIMARY KEY,
  kind    TEXT NOT NULL,
  host    TEXT NOT NULL UNIQUE COLLATE NOCASE,
  api_url TEXT NOT NULL
);
CREATE TABLE accounts (
  id             INTEGER PRIMARY KEY,
  provider_id    INTEGER NOT NULL REFERENCES providers(id),
  name           TEXT NOT NULL UNIQUE,
  login          TEXT NOT NULL,
  remote_user_id TEXT NOT NULL,
  source_kind    TEXT NOT NULL,
  source_value   TEXT NOT NULL DEFAULT '',
  created_at     TEXT NOT NULL
);
CREATE TABLE tracked (
  id          INTEGER PRIMARY KEY,
  provider_id INTEGER NOT NULL REFERENCES providers(id),
  account_id  INTEGER REFERENCES accounts(id),
  owner       TEXT NOT NULL COLLATE NOCASE,
  repo        TEXT NOT NULL DEFAULT '' COLLATE NOCASE,
  include     TEXT NOT NULL DEFAULT '[]',
  exclude     TEXT NOT NULL DEFAULT '[]',
  forks       INTEGER NOT NULL DEFAULT 0,
  archived    INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL,
  UNIQUE (provider_id, owner, repo)
);
CREATE TABLE repos (
  id               INTEGER PRIMARY KEY,
  provider_id      INTEGER NOT NULL REFERENCES providers(id),
  tracked_id       INTEGER REFERENCES tracked(id) ON DELETE SET NULL,
  remote_id        TEXT NOT NULL,
  owner            TEXT NOT NULL COLLATE NOCASE,
  name             TEXT NOT NULL COLLATE NOCASE,
  default_branch   TEXT NOT NULL,
  clone_url        TEXT NOT NULL,
  pushed_at        TEXT,
  fork             INTEGER NOT NULL DEFAULT 0,
  archived         INTEGER NOT NULL DEFAULT 0,
  state            TEXT NOT NULL DEFAULT 'active',
  head             TEXT,
  synced_pushed_at TEXT,
  synced_branch    TEXT,
  last_synced_at   TEXT,
  last_result      TEXT,
  last_message     TEXT,
  disk_owner       TEXT,
  disk_name        TEXT,
  UNIQUE (provider_id, remote_id)
);
CREATE INDEX repos_by_name ON repos (provider_id, owner, name);
CREATE TABLE sync_runs (
  id          INTEGER PRIMARY KEY,
  started_at  TEXT NOT NULL,
  finished_at TEXT,
  seconds     INTEGER,
  summary     TEXT
);
CREATE TABLE repo_syncs (
  id       INTEGER PRIMARY KEY,
  run_id   INTEGER NOT NULL REFERENCES sync_runs(id) ON DELETE CASCADE,
  repo_id  INTEGER REFERENCES repos(id) ON DELETE SET NULL,
  repo     TEXT NOT NULL,
  action   TEXT NOT NULL,
  old_head TEXT,
  new_head TEXT,
  message  TEXT
);
CREATE TABLE config (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
INSERT INTO providers (kind, host, api_url) VALUES
  ('github', 'github.com', 'https://api.github.com'),
  ('gitlab', 'gitlab.com', 'https://gitlab.com/api/v4'),
  ('bitbucket', 'bitbucket.org', 'https://api.bitbucket.org/2.0');
";

/// The directory holding `weir.db` and the sync lock.
pub fn home() -> PathBuf {
    match std::env::var_os("WEIR_HOME") {
        Some(h) if !h.is_empty() => PathBuf::from(h),
        _ => util::user_home().join(".weir"),
    }
}

pub fn open() -> Result<Connection> {
    let home = home();
    std::fs::create_dir_all(&home)
        .with_context(|| format!("could not create {}", home.display()))?;
    let path = home.join("weir.db");
    let conn =
        Connection::open(&path).with_context(|| format!("could not open {}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == 0 {
        conn.execute_batch(&format!("BEGIN; {SCHEMA} PRAGMA user_version = 1; COMMIT;"))?;
    }
    Ok(conn)
}

pub fn config_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM config WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()?)
}

pub fn config_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO config (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Where mirrors are kept. Defaults to `~/Repos`.
pub fn root(conn: &Connection) -> Result<PathBuf> {
    let raw = config_get(conn, "root")?.unwrap_or_else(|| "~/Repos".to_string());
    Ok(util::expand_tilde(&raw))
}

#[derive(Debug, Clone)]
pub struct ProviderRow {
    pub id: i64,
    pub kind: String,
    pub host: String,
    pub api_url: String,
}

fn provider_from(r: &Row) -> rusqlite::Result<ProviderRow> {
    Ok(ProviderRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        host: r.get(2)?,
        api_url: r.get(3)?,
    })
}

pub fn providers(conn: &Connection) -> Result<Vec<ProviderRow>> {
    let mut st = conn.prepare("SELECT id, kind, host, api_url FROM providers ORDER BY id")?;
    let rows = st.query_map([], provider_from)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn provider_by_host(conn: &Connection, host: &str) -> Result<Option<ProviderRow>> {
    Ok(conn
        .query_row(
            "SELECT id, kind, host, api_url FROM providers WHERE host = ?1",
            [host],
            provider_from,
        )
        .optional()?)
}

#[derive(Debug, Clone)]
pub struct AccountRow {
    pub id: i64,
    pub provider_id: i64,
    pub name: String,
    pub login: String,
    pub source_kind: String,
    pub source_value: String,
}

fn account_from(r: &Row) -> rusqlite::Result<AccountRow> {
    Ok(AccountRow {
        id: r.get(0)?,
        provider_id: r.get(1)?,
        name: r.get(2)?,
        login: r.get(3)?,
        source_kind: r.get(4)?,
        source_value: r.get(5)?,
    })
}

const ACCOUNT_COLS: &str = "id, provider_id, name, login, source_kind, source_value";

pub fn accounts(conn: &Connection) -> Result<Vec<AccountRow>> {
    let mut st = conn.prepare(&format!(
        "SELECT {ACCOUNT_COLS} FROM accounts ORDER BY provider_id, name"
    ))?;
    let rows = st.query_map([], account_from)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn account_by_name(conn: &Connection, name: &str) -> Result<Option<AccountRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {ACCOUNT_COLS} FROM accounts WHERE name = ?1"),
            [name],
            account_from,
        )
        .optional()?)
}

#[derive(Debug, Clone)]
pub struct TrackedRow {
    pub id: i64,
    pub provider_id: i64,
    pub account_id: Option<i64>,
    pub owner: String,
    /// Empty when the whole owner is tracked.
    pub repo: String,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub forks: bool,
    pub archived: bool,
}

pub fn tracked(conn: &Connection) -> Result<Vec<TrackedRow>> {
    let mut st = conn.prepare(
        "SELECT id, provider_id, account_id, owner, repo, include, exclude, forks, archived
         FROM tracked ORDER BY provider_id, owner, repo",
    )?;
    let rows = st.query_map([], |r| {
        let include: String = r.get(5)?;
        let exclude: String = r.get(6)?;
        Ok(TrackedRow {
            id: r.get(0)?,
            provider_id: r.get(1)?,
            account_id: r.get(2)?,
            owner: r.get(3)?,
            repo: r.get(4)?,
            include: serde_json::from_str(&include).unwrap_or_default(),
            exclude: serde_json::from_str(&exclude).unwrap_or_default(),
            forks: r.get(7)?,
            archived: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[derive(Debug, Clone)]
pub struct RepoRow {
    pub id: i64,
    pub provider_id: i64,
    pub tracked_id: Option<i64>,
    pub remote_id: String,
    pub owner: String,
    pub name: String,
    pub default_branch: String,
    pub clone_url: String,
    pub pushed_at: Option<String>,
    pub state: String,
    pub head: Option<String>,
    pub synced_pushed_at: Option<String>,
    pub synced_branch: Option<String>,
    pub last_synced_at: Option<String>,
    pub last_result: Option<String>,
    pub last_message: Option<String>,
    pub disk_owner: Option<String>,
    pub disk_name: Option<String>,
}

/// Repositories matching `filter`, a SQL condition over the `repos` columns.
pub fn repos(
    conn: &Connection,
    filter: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<RepoRow>> {
    let mut st = conn.prepare(&format!(
        "SELECT id, provider_id, tracked_id, remote_id, owner, name, default_branch, clone_url,
                pushed_at, state, head, synced_pushed_at, synced_branch, last_synced_at,
                last_result, last_message, disk_owner, disk_name
         FROM repos WHERE {filter} ORDER BY provider_id, owner, name"
    ))?;
    let rows = st.query_map(args, |r| {
        Ok(RepoRow {
            id: r.get(0)?,
            provider_id: r.get(1)?,
            tracked_id: r.get(2)?,
            remote_id: r.get(3)?,
            owner: r.get(4)?,
            name: r.get(5)?,
            default_branch: r.get(6)?,
            clone_url: r.get(7)?,
            pushed_at: r.get(8)?,
            state: r.get(9)?,
            head: r.get(10)?,
            synced_pushed_at: r.get(11)?,
            synced_branch: r.get(12)?,
            last_synced_at: r.get(13)?,
            last_result: r.get(14)?,
            last_message: r.get(15)?,
            disk_owner: r.get(16)?,
            disk_name: r.get(17)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
