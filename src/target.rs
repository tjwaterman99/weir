//! Turning what the user typed (`github.com/acme`, `acme/app`, `app`) into a
//! selection of providers, owners and repositories.

use anyhow::{bail, Result};
use rusqlite::Connection;

use crate::db::{self, ProviderRow, RepoRow, TrackedRow};

/// What a command applies to. `None` in a field means "any".
#[derive(Debug, Default)]
pub struct Scope {
    pub provider_id: Option<i64>,
    pub owner: Option<String>,
    pub repo: Option<String>,
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

impl Scope {
    pub fn matches_tracked(&self, t: &TrackedRow) -> bool {
        self.provider_id.is_none_or(|p| p == t.provider_id)
            && self.owner.as_deref().is_none_or(|o| same(o, &t.owner))
            && (t.repo.is_empty() || self.repo.as_deref().is_none_or(|r| same(r, &t.repo)))
    }

    pub fn matches_repo(&self, r: &RepoRow) -> bool {
        self.provider_id.is_none_or(|p| p == r.provider_id)
            && self.owner.as_deref().is_none_or(|o| same(o, &r.owner))
            && self.repo.as_deref().is_none_or(|n| same(n, &r.name))
    }
}

fn segments(target: &str) -> Result<Vec<&str>> {
    let parts: Vec<&str> = target.trim_matches('/').split('/').collect();
    if parts.len() > 3 || parts.iter().any(|p| p.is_empty()) {
        bail!("{target:?} is not a target; use <host>/<owner>[/<repo>]");
    }
    Ok(parts)
}

/// A target that must spell out the host, as `track` and `untrack` require.
pub fn parse_full(conn: &Connection, target: &str) -> Result<(ProviderRow, String, String)> {
    let parts = segments(target)?;
    if parts.len() < 2 {
        bail!("{target:?} needs a host and an owner, for example github.com/{target}");
    }
    let Some(provider) = db::provider_by_host(conn, parts[0])? else {
        let hosts: Vec<String> = db::providers(conn)?.into_iter().map(|p| p.host).collect();
        bail!(
            "{:?} is not a known provider host (known: {}). Add one with `weir provider add`.",
            parts[0],
            hosts.join(", ")
        );
    };
    let repo = parts.get(2).map(|s| s.to_string()).unwrap_or_default();
    Ok((provider, parts[1].to_string(), repo))
}

/// Resolves an optional target against what is in the database. Shorter forms
/// are accepted when they can only mean one thing.
pub fn resolve(conn: &Connection, target: Option<&str>) -> Result<Scope> {
    let Some(target) = target else {
        return Ok(Scope::default());
    };
    let parts = segments(target)?;
    let host = db::provider_by_host(conn, parts[0])?;
    match (parts.len(), host) {
        (3, Some(p)) => Ok(Scope {
            provider_id: Some(p.id),
            owner: Some(parts[1].to_string()),
            repo: Some(parts[2].to_string()),
        }),
        (3, None) => bail!("{:?} is not a known provider host", parts[0]),
        (2, Some(p)) => Ok(Scope {
            provider_id: Some(p.id),
            owner: Some(parts[1].to_string()),
            repo: None,
        }),
        (2, None) => Ok(Scope {
            provider_id: None,
            owner: Some(parts[0].to_string()),
            repo: Some(parts[1].to_string()),
        }),
        (_, Some(p)) => Ok(Scope {
            provider_id: Some(p.id),
            ..Scope::default()
        }),
        (_, None) => {
            let word = parts[0];
            let is_owner: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM tracked WHERE owner = ?1)
                     OR EXISTS (SELECT 1 FROM repos WHERE owner = ?1)",
                [word],
                |r| r.get(0),
            )?;
            let named = db::repos(conn, "name = ?1", &[&word])?;
            match (is_owner, named.len()) {
                // An owner wins over a repository of the same name, which is
                // common: a profile repository is named after its owner.
                (true, _) => Ok(Scope {
                    owner: Some(word.to_string()),
                    ..Scope::default()
                }),
                (false, 1) => Ok(Scope {
                    provider_id: Some(named[0].provider_id),
                    owner: Some(named[0].owner.clone()),
                    repo: Some(named[0].name.clone()),
                }),
                (false, 0) => bail!("nothing known matches {word:?}"),
                _ => bail!("{word:?} is ambiguous; use <owner>/<repo> or <host>/<owner>"),
            }
        }
    }
}

/// The single repository a name refers to, for `weir path`.
pub fn one_repo(conn: &Connection, target: &str) -> Result<RepoRow> {
    let parts = segments(target)?;
    let found = match parts.len() {
        1 => db::repos(conn, "name = ?1", &[&parts[0]])?,
        2 => db::repos(conn, "owner = ?1 AND name = ?2", &[&parts[0], &parts[1]])?,
        _ => {
            let Some(p) = db::provider_by_host(conn, parts[0])? else {
                bail!("{:?} is not a known provider host", parts[0]);
            };
            db::repos(
                conn,
                "provider_id = ?1 AND owner = ?2 AND name = ?3",
                &[&p.id, &parts[1], &parts[2]],
            )?
        }
    };
    // Prefer live mirrors, then archived ones, over repositories that are gone.
    let in_state =
        |state: &str| -> Vec<&RepoRow> { found.iter().filter(|r| r.state == state).collect() };
    let pick: Vec<&RepoRow> = [in_state("active"), in_state("archived")]
        .into_iter()
        .find(|v| !v.is_empty())
        .unwrap_or_else(|| found.iter().collect());
    match pick.len() {
        0 => bail!("no repository named {target:?} is known; see `weir list`"),
        1 => Ok(pick[0].clone()),
        _ => {
            let names: Vec<String> = pick
                .iter()
                .map(|r| format!("{}/{}", r.owner, r.name))
                .collect();
            bail!("{target:?} is ambiguous: {}", names.join(", "))
        }
    }
}
