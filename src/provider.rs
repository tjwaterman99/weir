//! Providers: the places repositories come from.
//!
//! A provider answers three questions: who does this token belong to, which
//! repositories does an owner have, and where is one repository cloned from.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::db::ProviderRow;
use crate::git;

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteRepo {
    /// The provider's own id for the repository. It survives renames.
    pub id: String,
    pub owner: String,
    pub name: String,
    pub default_branch: String,
    pub clone_url: String,
    /// Changes whenever the repository receives a push. Compared, never parsed.
    pub pushed_at: Option<String>,
    pub fork: bool,
    pub archived: bool,
    /// Size in kilobytes, when the provider reports one.
    pub size_kb: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Identity {
    pub login: String,
    pub id: String,
}

/// The provider rejected the token.
#[derive(Debug)]
pub struct AuthError(pub String);

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AuthError {}

pub trait Provider {
    /// Whether repositories are listed with an account's token.
    fn needs_account(&self) -> bool {
        true
    }

    /// The `host:port` to reach before listing, if the provider is remote.
    fn endpoint(&self) -> Option<String> {
        None
    }

    fn whoami(&self, token: &str) -> Result<Identity>;

    /// Every repository of `owner` that the token can see. `login` is the
    /// account's own login, which lets a provider include private repositories
    /// when the owner is the account itself.
    fn list_repos(
        &self,
        owner: &str,
        token: Option<&str>,
        login: Option<&str>,
    ) -> Result<Vec<RemoteRepo>>;

    /// What `owner/name` refers to today. Providers redirect the old name of a
    /// renamed repository, so this also resolves names that are out of date.
    fn lookup(&self, owner: &str, name: &str, token: Option<&str>) -> Result<Lookup>;

    /// The repository with this provider id, wherever it lives now.
    fn lookup_id(&self, id: &str, token: Option<&str>) -> Result<Lookup>;

    fn get_repo(&self, owner: &str, name: &str, token: Option<&str>) -> Result<Option<RemoteRepo>> {
        match self.lookup(owner, name, token)? {
            Lookup::Found(repo) => Ok(Some(*repo)),
            Lookup::NotFound => Ok(None),
            Lookup::Forbidden => bail!("access to {owner}/{name} is forbidden"),
        }
    }
}

/// The answer to "does this repository exist?".
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    Found(Box<RemoteRepo>),
    /// It does not exist, or the provider will not admit that it does. GitHub
    /// answers "not found" for a private repository the token cannot see.
    NotFound,
    /// It exists, but this token may not read it.
    Forbidden,
}

pub fn build(p: &ProviderRow) -> Result<Box<dyn Provider>> {
    match p.kind.as_str() {
        "github" => Ok(Box::new(GitHub::new(&p.api_url))),
        "local" => Ok(Box::new(Local {
            dir: PathBuf::from(&p.api_url),
        })),
        other => bail!("{other} providers are not supported yet ({})", p.host),
    }
}

// ---------------------------------------------------------------- GitHub

pub struct GitHub {
    api: String,
    agent: ureq::Agent,
}

#[derive(Deserialize)]
struct GhOwner {
    login: String,
}

#[derive(Deserialize)]
struct GhRepo {
    id: u64,
    name: String,
    owner: GhOwner,
    default_branch: Option<String>,
    clone_url: String,
    pushed_at: Option<String>,
    #[serde(default)]
    fork: bool,
    #[serde(default)]
    archived: bool,
    size: Option<u64>,
}

impl From<GhRepo> for RemoteRepo {
    fn from(r: GhRepo) -> RemoteRepo {
        RemoteRepo {
            id: r.id.to_string(),
            owner: r.owner.login,
            name: r.name,
            default_branch: r.default_branch.unwrap_or_else(|| "main".to_string()),
            clone_url: r.clone_url,
            pushed_at: r.pushed_at,
            fork: r.fork,
            archived: r.archived,
            size_kb: r.size,
        }
    }
}

#[derive(Deserialize)]
struct GhUser {
    login: String,
    #[serde(default)]
    id: u64,
    #[serde(default, rename = "type")]
    kind: String,
}

enum Fetched {
    Ok(Box<ureq::Response>),
    NotFound,
    Forbidden,
}

impl GitHub {
    pub fn new(api: &str) -> GitHub {
        GitHub {
            api: api.trim_end_matches('/').to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(30))
                // A renamed repository answers with a redirect to its id. The
                // token has to follow it, or a private repository looks gone.
                .redirect_auth_headers(ureq::RedirectAuthHeaders::SameHost)
                .build(),
        }
    }

    fn get(&self, url: &str, token: Option<&str>) -> Result<Fetched> {
        let mut req = self
            .agent
            .get(url)
            .set("Accept", "application/vnd.github+json")
            .set("X-GitHub-Api-Version", "2022-11-28")
            .set("User-Agent", "weir");
        if let Some(t) = token {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        match req.call() {
            Ok(resp) => Ok(Fetched::Ok(Box::new(resp))),
            Err(ureq::Error::Status(401, _)) => {
                Err(AuthError("the token was rejected (HTTP 401)".to_string()).into())
            }
            Err(ureq::Error::Status(404, _)) => Ok(Fetched::NotFound),
            // 403 also means "rate limited", which says nothing about access.
            Err(ureq::Error::Status(403 | 451, resp))
                if resp.header("x-ratelimit-remaining") != Some("0")
                    && resp.header("retry-after").is_none() =>
            {
                Ok(Fetched::Forbidden)
            }
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                let message = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
                    .unwrap_or(body);
                bail!("HTTP {code} from {url}: {}", message.trim())
            }
            Err(e) => Err(anyhow!("could not reach {url}: {e}")),
        }
    }

    fn pages(&self, first: String, token: Option<&str>) -> Result<Vec<RemoteRepo>> {
        let mut out = Vec::new();
        let mut next = Some(first);
        while let Some(url) = next {
            let resp = match self.get(&url, token)? {
                Fetched::Ok(r) => r,
                Fetched::NotFound => bail!("not found: {url}"),
                Fetched::Forbidden => bail!("access to {url} is forbidden"),
            };
            next = resp.header("link").and_then(parse_link_next);
            let page: Vec<GhRepo> = resp
                .into_json()
                .with_context(|| format!("unexpected response from {url}"))?;
            out.extend(page.into_iter().map(RemoteRepo::from));
        }
        Ok(out)
    }
}

/// The `rel="next"` URL from a `Link` header, if there is one.
pub fn parse_link_next(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let (url, rel) = part.split_once(';')?;
        if !rel.contains("rel=\"next\"") {
            return None;
        }
        Some(
            url.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string(),
        )
    })
}

impl Provider for GitHub {
    fn endpoint(&self) -> Option<String> {
        let (scheme, rest) = self.api.split_once("://")?;
        let authority = rest.split('/').next()?;
        if authority.contains(':') {
            Some(authority.to_string())
        } else {
            let port = if scheme == "http" { 80 } else { 443 };
            Some(format!("{authority}:{port}"))
        }
    }

    fn whoami(&self, token: &str) -> Result<Identity> {
        let url = format!("{}/user", self.api);
        match self.get(&url, Some(token))? {
            Fetched::Ok(resp) => {
                let user: GhUser = resp.into_json().context("unexpected response from /user")?;
                Ok(Identity {
                    login: user.login,
                    id: user.id.to_string(),
                })
            }
            Fetched::NotFound => bail!("not found: {url}"),
            Fetched::Forbidden => bail!("access to {url} is forbidden"),
        }
    }

    fn list_repos(
        &self,
        owner: &str,
        token: Option<&str>,
        login: Option<&str>,
    ) -> Result<Vec<RemoteRepo>> {
        let url = format!("{}/users/{owner}", self.api);
        let user: GhUser = match self.get(&url, token)? {
            Fetched::Ok(resp) => resp.into_json().context("unexpected response")?,
            Fetched::NotFound => bail!("no user or organization named {owner}"),
            Fetched::Forbidden => bail!("access to {owner} is forbidden"),
        };
        let first = if user.kind == "Organization" {
            format!("{}/orgs/{owner}/repos?type=all&per_page=100", self.api)
        } else if login.is_some_and(|l| l.eq_ignore_ascii_case(&user.login)) {
            // Only this endpoint includes the account's private repositories.
            format!("{}/user/repos?affiliation=owner&per_page=100", self.api)
        } else {
            format!("{}/users/{owner}/repos?type=owner&per_page=100", self.api)
        };
        self.pages(first, token)
    }

    fn lookup(&self, owner: &str, name: &str, token: Option<&str>) -> Result<Lookup> {
        self.find(&format!("{}/repos/{owner}/{name}", self.api), token)
    }

    fn lookup_id(&self, id: &str, token: Option<&str>) -> Result<Lookup> {
        self.find(&format!("{}/repositories/{id}", self.api), token)
    }
}

impl GitHub {
    fn find(&self, url: &str, token: Option<&str>) -> Result<Lookup> {
        match self.get(url, token)? {
            Fetched::Ok(resp) => {
                let repo: GhRepo = resp.into_json().context("unexpected response")?;
                Ok(Lookup::Found(Box::new(repo.into())))
            }
            Fetched::NotFound => Ok(Lookup::NotFound),
            Fetched::Forbidden => Ok(Lookup::Forbidden),
        }
    }
}

// ----------------------------------------------------------------- local

/// A directory of bare repositories, laid out as `<dir>/<owner>/<name>.git`.
///
/// It needs no account and no network, which makes it the provider the test
/// suite runs against. It also works for repositories kept on a shared disk.
/// Optional marker files inside a repository let tests imitate a real provider:
/// `weir-id` (a stable id, so a rename is seen as a rename), `weir-archived`,
/// `weir-fork`, `weir-size` (kilobytes), `weir-old-names` (one `owner/name` per line that redirects
/// here), `weir-hidden` (exists but is not listed) and `weir-forbidden`
/// (exists, not listed, and lookups are refused).
pub struct Local {
    dir: PathBuf,
}

impl Local {
    fn describe(&self, owner: &str, path: &Path) -> Result<RemoteRepo> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".git"))
            .ok_or_else(|| anyhow!("{} is not a bare repository", path.display()))?
            .to_string();
        let id = std::fs::read_to_string(path.join("weir-id"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| format!("{owner}/{name}"));
        let default_branch = git::opt(Some(path), &["symbolic-ref", "--short", "HEAD"])
            .unwrap_or_else(|| "main".to_string());
        Ok(RemoteRepo {
            id,
            owner: owner.to_string(),
            default_branch,
            clone_url: path.to_string_lossy().into_owned(),
            pushed_at: git::opt(Some(path), &["rev-parse", "-q", "--verify", "HEAD"]),
            fork: path.join("weir-fork").exists(),
            archived: path.join("weir-archived").exists(),
            size_kb: std::fs::read_to_string(path.join("weir-size"))
                .ok()
                .and_then(|s| s.trim().parse().ok()),
            name,
        })
    }
}

impl Provider for Local {
    fn needs_account(&self) -> bool {
        false
    }

    fn whoami(&self, _token: &str) -> Result<Identity> {
        bail!("local providers have no accounts")
    }

    fn list_repos(
        &self,
        owner: &str,
        _token: Option<&str>,
        _login: Option<&str>,
    ) -> Result<Vec<RemoteRepo>> {
        let dir = self.dir.join(owner);
        let entries =
            std::fs::read_dir(&dir).with_context(|| format!("could not read {}", dir.display()))?;
        let mut out = Vec::new();
        for entry in entries {
            let path = entry?.path();
            let listed =
                !path.join("weir-hidden").exists() && !path.join("weir-forbidden").exists();
            if listed && path.is_dir() && path.extension().is_some_and(|e| e == "git") {
                out.push(self.describe(owner, &path)?);
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn lookup(&self, owner: &str, name: &str, _token: Option<&str>) -> Result<Lookup> {
        let path = self.dir.join(owner).join(format!("{name}.git"));
        if path.is_dir() {
            return self.answer(owner, &path);
        }
        let old = format!("{owner}/{name}");
        for (current_owner, path) in self.all()? {
            let names = std::fs::read_to_string(path.join("weir-old-names")).unwrap_or_default();
            if names.lines().any(|l| l.trim().eq_ignore_ascii_case(&old)) {
                return self.answer(&current_owner, &path);
            }
        }
        Ok(Lookup::NotFound)
    }

    fn lookup_id(&self, id: &str, _token: Option<&str>) -> Result<Lookup> {
        for (owner, path) in self.all()? {
            if self.describe(&owner, &path)?.id == id {
                return self.answer(&owner, &path);
            }
        }
        Ok(Lookup::NotFound)
    }
}

impl Local {
    /// Every bare repository under the directory, with its owner.
    fn all(&self) -> Result<Vec<(String, PathBuf)>> {
        let mut out = Vec::new();
        for owner in std::fs::read_dir(&self.dir)?.flatten() {
            let Ok(repos) = std::fs::read_dir(owner.path()) else {
                continue;
            };
            let owner = owner.file_name().to_string_lossy().into_owned();
            for repo in repos.flatten() {
                let path = repo.path();
                if path.is_dir() && path.extension().is_some_and(|e| e == "git") {
                    out.push((owner.clone(), path));
                }
            }
        }
        out.sort();
        Ok(out)
    }

    fn answer(&self, owner: &str, path: &Path) -> Result<Lookup> {
        if path.join("weir-forbidden").exists() {
            Ok(Lookup::Forbidden)
        } else {
            Ok(Lookup::Found(Box::new(self.describe(owner, path)?)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_next_page() {
        let h = "<https://api.github.com/orgs/x/repos?page=2>; rel=\"next\", \
                 <https://api.github.com/orgs/x/repos?page=9>; rel=\"last\"";
        assert_eq!(
            parse_link_next(h).as_deref(),
            Some("https://api.github.com/orgs/x/repos?page=2")
        );
        assert_eq!(parse_link_next("<https://x/y?page=1>; rel=\"prev\""), None);
    }

    /// Answers each request with what `route` returns: status, Location, body.
    fn serve(route: fn(&str, bool) -> (u16, Option<&'static str>, &'static str)) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let root = base.clone();
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
                let request = String::from_utf8_lossy(&raw).to_lowercase();
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let authorized = request.contains("authorization: bearer good");
                let (code, location, body) = route(&path, authorized);
                let location = location
                    .map(|l| format!("Location: {root}{l}\r\n"))
                    .unwrap_or_default();
                let _ = write!(
                    stream,
                    "HTTP/1.1 {code} X\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        base
    }

    const REPO: &str = r#"{"id":7,"name":"new","owner":{"login":"acme"},"default_branch":"main","clone_url":"https://example.test/acme/new.git","pushed_at":"t","fork":false,"archived":false}"#;

    #[test]
    fn lookup_follows_a_rename_and_keeps_the_token() {
        let base = serve(|path, authorized| match (path, authorized) {
            ("/repos/acme/old", true) => (301, Some("/repositories/7"), ""),
            ("/repositories/7", true) => (200, None, REPO),
            ("/repos/acme/secret", true) => (403, None, r#"{"message":"SSO required"}"#),
            _ => (404, None, r#"{"message":"Not Found"}"#),
        });
        let gh = GitHub::new(&base);
        match gh.lookup("acme", "old", Some("good")).unwrap() {
            Lookup::Found(repo) => assert_eq!((repo.id.as_str(), repo.name.as_str()), ("7", "new")),
            other => panic!("expected the renamed repository, got {other:?}"),
        }
        assert_eq!(
            gh.lookup_id("7", Some("good")).unwrap(),
            gh.lookup("acme", "old", Some("good")).unwrap()
        );
        assert_eq!(
            gh.lookup("acme", "nope", Some("good")).unwrap(),
            Lookup::NotFound
        );
        assert_eq!(
            gh.lookup("acme", "secret", Some("good")).unwrap(),
            Lookup::Forbidden
        );
    }

    #[test]
    fn github_endpoint() {
        assert_eq!(
            GitHub::new("https://api.github.com").endpoint().as_deref(),
            Some("api.github.com:443")
        );
        assert_eq!(
            GitHub::new("http://127.0.0.1:8080/api/v3")
                .endpoint()
                .as_deref(),
            Some("127.0.0.1:8080")
        );
    }
}
