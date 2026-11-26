//! Reading the history of sync runs, for `status`, `log` and `watch`.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeMap;

use crate::sync::Event;

#[derive(Debug, Clone)]
pub struct RunRow {
    pub id: i64,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub seconds: Option<i64>,
    pub summary: Option<String>,
    pub phase: Option<String>,
    pub planned: Option<i64>,
    pub done: i64,
    pub unchanged: i64,
    pub current: Option<String>,
    pub ok: Option<bool>,
    pub listing_seconds: Option<i64>,
}

impl RunRow {
    fn summary_json(&self) -> serde_json::Value {
        self.summary
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    }

    /// How many repositories ended in each outcome.
    pub fn counts(&self) -> BTreeMap<String, usize> {
        self.summary_json()
            .as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n as usize)))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn problems(&self) -> Vec<String> {
        self.summary_json()["problems"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| p.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The repositories being worked on, with how long each has taken so far.
    pub fn current(&self) -> Vec<(String, u64)> {
        self.current
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|c| Some((c["repo"].as_str()?.to_string(), c["seconds"].as_u64()?)))
            .collect()
    }

    /// One word for how the run ended.
    pub fn verdict(&self) -> &'static str {
        match (self.phase.as_deref(), self.finished_at.is_some(), self.ok) {
            (Some("interrupted"), _, _) => "interrupted",
            (Some("failed"), _, _) => "failed",
            (_, false, _) => "unfinished",
            (_, true, Some(true)) => "ok",
            (_, true, _) => "problems",
        }
    }
}

/// The most recent runs, newest first.
pub fn latest(conn: &Connection, count: usize) -> Result<Vec<RunRow>> {
    let mut st = conn.prepare(
        "SELECT id, started_at, finished_at, seconds, summary, phase, planned, done, unchanged,
                current, ok, listing_seconds
         FROM sync_runs ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = st.query_map([count as i64], |r| {
        Ok(RunRow {
            id: r.get(0)?,
            started_at: r.get(1)?,
            finished_at: r.get(2)?,
            seconds: r.get(3)?,
            summary: r.get(4)?,
            phase: r.get(5)?,
            planned: r.get(6)?,
            done: r.get(7)?,
            unchanged: r.get(8)?,
            current: r.get(9)?,
            ok: r.get(10)?,
            listing_seconds: r.get(11)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// What a run has recorded so far, oldest first. `after` skips events already seen.
pub fn events(conn: &Connection, run_id: i64, after: i64) -> Result<Vec<(i64, Event)>> {
    let mut st = conn.prepare(
        "SELECT id, action, repo, old_head, new_head, message, millis
         FROM repo_syncs WHERE run_id = ?1 AND id > ?2 ORDER BY id",
    )?;
    let rows = st.query_map([run_id, after], |r| {
        Ok((
            r.get(0)?,
            Event {
                action: r.get(1)?,
                repo: r.get(2)?,
                repo_id: None,
                old_head: r.get(3)?,
                new_head: r.get(4)?,
                message: r.get(5)?,
                millis: r.get(6)?,
            },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Counts by outcome for a run that is still going, read from what it has
/// recorded so far.
pub fn live_counts(conn: &Connection, run: &RunRow) -> Result<BTreeMap<String, usize>> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (_, e) in events(conn, run.id, 0)? {
        *counts.entry(e.action).or_default() += 1;
    }
    if run.unchanged > 0 {
        *counts.entry("unchanged".to_string()).or_default() += run.unchanged as usize;
    }
    Ok(counts)
}
