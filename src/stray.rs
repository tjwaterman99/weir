//! Git repositories under the root that weir does not manage, and why.
//!
//! They are the clones that quietly go stale: left behind by a rename, made
//! by hand, or belonging to something that is no longer tracked.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::db::{self, ProviderRow};
use crate::git;
use crate::provider::{self, Lookup};
use crate::secret::Source;
use crate::sync::mirror_path;

#[derive(Debug, Clone)]
pub struct Stray {
    pub provider_id: i64,
    pub host: String,
    pub owner: String,
    pub name: String,
    pub path: PathBuf,
}

impl Stray {
    pub fn label(&self) -> String {
        format!("{}/{}/{}", self.host, self.owner, self.name)
    }
}

/// Every git repository at `<root>/<host>/<owner>/<name>` that no row in the
/// database accounts for. Reads the disk only.
pub fn scan(conn: &Connection, root: &Path) -> Result<Vec<Stray>> {
    let mut known: HashSet<(i64, String)> = HashSet::new();
    for r in db::repos(conn, "1 = 1", &[])? {
        known.insert((
            r.provider_id,
            format!("{}/{}", r.owner, r.name).to_lowercase(),
        ));
        if let (Some(o), Some(n)) = (&r.disk_owner, &r.disk_name) {
            known.insert((r.provider_id, format!("{o}/{n}").to_lowercase()));
        }
    }
    let mut out = Vec::new();
    for p in db::providers(conn)? {
        let Ok(owners) = std::fs::read_dir(root.join(&p.host)) else {
            continue;
        };
        for owner in owners.flatten() {
            let Ok(names) = std::fs::read_dir(owner.path()) else {
                continue;
            };
            let owner = owner.file_name().to_string_lossy().into_owned();
            for entry in names.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                // A half-finished clone of weir's own is not a stray.
                if !path.join(".git").is_dir() || name.starts_with(git::CLONE_PREFIX) {
                    continue;
                }
                if known.contains(&(p.id, format!("{owner}/{name}").to_lowercase())) {
                    continue;
                }
                out.push(Stray {
                    provider_id: p.id,
                    host: p.host.clone(),
                    owner: owner.clone(),
                    name,
                    path,
                });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// The `owner/name` a remote URL points at, when it belongs to this provider.
pub fn origin_owner_name(p: &ProviderRow, origin: &str) -> Option<(String, String)> {
    let url = git::normalize_url(origin);
    let prefix = if p.kind == "local" {
        git::normalize_url(&p.api_url)
    } else {
        p.host.to_lowercase()
    };
    let rest = url.strip_prefix(&prefix)?.strip_prefix('/')?;
    let (owner, name) = rest.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some((owner.to_string(), name.to_string()))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// It has no `origin` remote, so it exists nowhere else.
    NoRemote,
    /// Its origin is not a repository on this provider.
    Foreign(String),
    /// A leftover copy of a repository whose mirror is somewhere else.
    Duplicate { repo: String, mirror: PathBuf },
    /// A repository weir will manage: the next sync adopts this or moves it into place.
    Pending(String),
    /// It exists upstream under a tracked owner but the filters leave it out.
    Excluded(String),
    /// It exists upstream but nothing tracks it.
    Untracked(String),
    /// The provider has no such repository (or will not say so).
    Gone,
    /// It exists but the account may not read it.
    Inaccessible,
    /// The provider could not be asked.
    Unknown(String),
}

impl Kind {
    pub fn label(&self) -> &'static str {
        match self {
            Kind::NoRemote => "no-remote",
            Kind::Foreign(_) => "foreign",
            Kind::Duplicate { .. } => "duplicate",
            Kind::Pending(_) => "pending",
            Kind::Excluded(_) => "excluded",
            Kind::Untracked(_) => "untracked",
            Kind::Gone => "gone",
            Kind::Inaccessible => "inaccessible",
            Kind::Unknown(_) => "unknown",
        }
    }

    pub fn explain(&self) -> String {
        match self {
            Kind::NoRemote => "has no origin remote; this may be the only copy".to_string(),
            Kind::Foreign(origin) => format!("a clone of {origin}, which is not on this provider"),
            Kind::Duplicate { repo, mirror } => format!(
                "a stale copy of {repo}, whose mirror is {}",
                mirror.display()
            ),
            Kind::Pending(repo) => format!("is {repo}; the next sync takes it over"),
            Kind::Excluded(repo) => format!("is {repo}, which your filters leave out"),
            Kind::Untracked(repo) => format!("is {repo}, which is not tracked"),
            Kind::Gone => {
                "not found on the provider (deleted, or no longer visible); this may be the only copy"
                    .to_string()
            }
            Kind::Inaccessible => "exists on the provider but access is refused".to_string(),
            Kind::Unknown(why) => format!("could not be checked: {why}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Report {
    pub stray: Stray,
    pub kind: Kind,
}

/// Asks each provider what the strays are. One request per stray.
pub fn classify(conn: &Connection, root: &Path, strays: &[Stray]) -> Result<Vec<Report>> {
    let providers: HashMap<i64, ProviderRow> = db::providers(conn)?
        .into_iter()
        .map(|p| (p.id, p))
        .collect();
    let accounts = db::accounts(conn)?;
    let tracked = db::tracked(conn)?;
    let mut tokens: HashMap<i64, Result<String, String>> = HashMap::new();
    let mut out = Vec::new();

    for stray in strays {
        let p = &providers[&stray.provider_id];
        let kind = (|| -> Result<Kind> {
            let Some(origin) =
                git::opt(Some(&stray.path), &["config", "--get", "remote.origin.url"])
            else {
                return Ok(Kind::NoRemote);
            };
            let Some((owner, name)) = origin_owner_name(p, &origin) else {
                return Ok(Kind::Foreign(origin));
            };
            let prov = provider::build(p)?;
            let token = if prov.needs_account() {
                // Prefer the account that tracks this owner; any account of the host will do.
                let account = tracked
                    .iter()
                    .find(|t| t.provider_id == p.id && t.owner.eq_ignore_ascii_case(&owner))
                    .and_then(|t| t.account_id)
                    .and_then(|id| accounts.iter().find(|a| a.id == id))
                    .or_else(|| accounts.iter().find(|a| a.provider_id == p.id));
                let Some(account) = account else {
                    return Ok(Kind::Unknown(format!("{} has no account", p.host)));
                };
                let token = tokens.entry(account.id).or_insert_with(|| {
                    Source::from_db(&account.source_kind, &account.source_value)
                        .and_then(|s| s.resolve(&account.name))
                        .map_err(|e| format!("{e:#}"))
                });
                match token {
                    Ok(t) => Some(t.clone()),
                    Err(e) => return Ok(Kind::Unknown(e.clone())),
                }
            } else {
                None
            };
            Ok(match prov.lookup(&owner, &name, token.as_deref())? {
                Lookup::NotFound => Kind::Gone,
                Lookup::Forbidden => Kind::Inaccessible,
                Lookup::Found(repo) => {
                    let full = format!("{}/{}/{}", p.host, repo.owner, repo.name);
                    let row = db::repos(
                        conn,
                        "provider_id = ?1 AND remote_id = ?2",
                        &[&p.id, &repo.id],
                    )?
                    .pop();
                    match row {
                        Some(row) => {
                            let mirror = mirror_path(root, &p.host, &row.owner, &row.name);
                            if mirror.exists() && mirror != stray.path {
                                Kind::Duplicate { repo: full, mirror }
                            } else {
                                Kind::Pending(full)
                            }
                        }
                        None => {
                            let owner_tracked = tracked.iter().any(|t| {
                                t.provider_id == p.id
                                    && t.repo.is_empty()
                                    && t.owner.eq_ignore_ascii_case(&repo.owner)
                            });
                            let expected = mirror_path(root, &p.host, &repo.owner, &repo.name);
                            if owner_tracked && repo.archived && expected == stray.path {
                                // An archived repository with a clone already in place.
                                Kind::Pending(full)
                            } else if owner_tracked {
                                Kind::Excluded(full)
                            } else {
                                Kind::Untracked(full)
                            }
                        }
                    }
                }
            })
        })()
        .unwrap_or_else(|e| Kind::Unknown(format!("{e:#}")));
        out.push(Report {
            stray: stray.clone(),
            kind,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(kind: &str, host: &str, api_url: &str) -> ProviderRow {
        ProviderRow {
            id: 1,
            kind: kind.to_string(),
            host: host.to_string(),
            api_url: api_url.to_string(),
        }
    }

    #[test]
    fn reads_owner_and_name_from_an_origin() {
        let gh = provider("github", "github.com", "https://api.github.com");
        let want = Some(("acme".to_string(), "app".to_string()));
        assert_eq!(
            origin_owner_name(&gh, "https://github.com/Acme/App.git"),
            want
        );
        assert_eq!(origin_owner_name(&gh, "git@github.com:acme/app.git"), want);
        assert_eq!(
            origin_owner_name(&gh, "https://gitlab.com/acme/app.git"),
            None
        );
        assert_eq!(
            origin_owner_name(&gh, "https://github.com/acme/app/extra"),
            None
        );

        let local = provider("local", "local.test", "/srv/git");
        assert_eq!(origin_owner_name(&local, "/srv/git/acme/app.git"), want);
        assert_eq!(origin_owner_name(&local, "/elsewhere/acme/app.git"), None);
    }
}
