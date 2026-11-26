//! Where an account's token lives. The database stores only this reference.

use anyhow::{bail, Context, Result};
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Keychain,
    Env(String),
    Command(String),
}

impl Source {
    pub fn from_db(kind: &str, value: &str) -> Result<Source> {
        Ok(match kind {
            "keychain" => Source::Keychain,
            "env" => Source::Env(value.to_string()),
            "command" => Source::Command(value.to_string()),
            other => bail!("unknown secret source {other:?}"),
        })
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Source::Keychain => "keychain",
            Source::Env(_) => "env",
            Source::Command(_) => "command",
        }
    }

    pub fn value(&self) -> &str {
        match self {
            Source::Keychain => "",
            Source::Env(v) | Source::Command(v) => v,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Source::Keychain => "keychain".to_string(),
            Source::Env(v) => format!("env: {v}"),
            Source::Command(v) => format!("command: {v}"),
        }
    }

    /// Reads the token for `account` from wherever it is kept.
    pub fn resolve(&self, account: &str) -> Result<String> {
        let token = match self {
            Source::Keychain => keychain::get(account)
                .with_context(|| format!("could not read the keychain entry for {account}"))?,
            Source::Env(var) => std::env::var(var)
                .map_err(|_| anyhow::anyhow!("environment variable {var} is not set"))?,
            Source::Command(cmd) => {
                let out = Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .output()
                    .with_context(|| format!("could not run `{cmd}`"))?;
                if !out.status.success() {
                    bail!(
                        "`{cmd}` failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
                String::from_utf8_lossy(&out.stdout).into_owned()
            }
        };
        let token = token.trim().to_string();
        if token.is_empty() {
            bail!("the token from {} is empty", self.describe());
        }
        Ok(token)
    }
}

pub use keychain::{delete as keychain_delete, set as keychain_set};

#[cfg(target_os = "macos")]
mod keychain {
    use anyhow::Result;

    const SERVICE: &str = "weir";

    pub fn set(account: &str, token: &str) -> Result<()> {
        keyring::Entry::new(SERVICE, account)?.set_password(token)?;
        Ok(())
    }

    pub fn get(account: &str) -> Result<String> {
        Ok(keyring::Entry::new(SERVICE, account)?.get_password()?)
    }

    pub fn delete(account: &str) -> Result<()> {
        match keyring::Entry::new(SERVICE, account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod keychain {
    use anyhow::{bail, Result};

    const HINT: &str = "the keychain is only available on macOS; use --env or --command";

    pub fn set(_account: &str, _token: &str) -> Result<()> {
        bail!(HINT)
    }

    pub fn get(_account: &str) -> Result<String> {
        bail!(HINT)
    }

    pub fn delete(_account: &str) -> Result<()> {
        Ok(())
    }
}
