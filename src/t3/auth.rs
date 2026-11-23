//! The read-only T3 tokens, one per environment: issued by T3's CLI, locally or over SSH on a
//! remote, kept with DPAPI, shown to nobody.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use super::http::{Api, Session, server_address};
use super::{Failure, Source, catalog, reach, ssh};
use crate::log::log;
use crate::win;

const SCOPE: &str = "orchestration:read";
const LABEL: &str = "t3-dock";
const TTL: &str = "30d";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Finds the remote's T3 CLI the way the desktop client runs it: an installed `t3`, else the
/// runner the client left in `ssh-launch`. The scope is fixed here, as for the local CLI.
const REMOTE_ISSUE_SCRIPT: &str = r#"R="$HOME/.t3/bin/t3"
[ -x "$R" ] || R="$(command -v t3 2>/dev/null || true)"
[ -n "$R" ] || R="$(ls -t "$HOME"/.t3/ssh-launch/*/run-t3.sh 2>/dev/null | head -n 1)"
[ -n "$R" ] || exit 3
exec "$R" auth session issue --scope orchestration:read --label t3-dock --ttl 30d --token-only"#;

/// Implements neither `Debug` nor `Display`, so it cannot be formatted into a log.
pub struct Token(String);

impl Token {
    fn parse(raw: &str) -> Option<Self> {
        let value = raw.trim();
        // The CLI prints only the token. Anything with spaces or several lines is something else.
        let valid =
            (16..=8192).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_graphic());
        valid.then(|| Self(value.to_owned()))
    }

    pub(super) fn bearer(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

pub fn load(path: &Path) -> Result<Token, Failure> {
    let sealed = std::fs::read(path).map_err(|_| Failure::NoToken)?;
    let plain = win::unprotect(&sealed).ok_or(Failure::NoToken)?;
    std::str::from_utf8(&plain)
        .ok()
        .and_then(Token::parse)
        .ok_or(Failure::NoToken)
}

/// Accepts a live session with exactly the read scope, and refuses a broader one.
pub fn check(session: &Session) -> Result<(), Failure> {
    if !session.authenticated {
        return Err(Failure::Unauthorized);
    }
    match session.scopes.as_deref() {
        Some([scope]) if scope == SCOPE => Ok(()),
        _ => Err(Failure::WrongScope),
    }
}

/// `t3-dock auth`: for the local server and each remote one in T3's catalog, keeps a working
/// token, otherwise issues one. One environment failing does not stop the others.
pub fn setup(t3_home: &Path, data: &Path) -> Result<(), String> {
    let mut failed = Vec::new();
    if let Err(error) = setup_one(t3_home, data, &Source::Local) {
        log!("auth: {error}");
        failed.push("local");
    }
    let remotes = match catalog::load(t3_home) {
        Ok(found) => found.remotes,
        Err(catalog::CatalogError::Missing) => Vec::new(),
        Err(error) => {
            log!("auth: {error}");
            failed.push("catalog");
            Vec::new()
        }
    };
    // Numbered as the running app numbers them.
    for (index, remote) in remotes.into_iter().enumerate() {
        let name = format!("t3 remote {}", index + 1);
        let source = Source::Remote { name, remote };
        if let Err(error) = setup_one(t3_home, data, &source) {
            log!("auth {}: {error}", source.name());
            failed.push("remote");
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} of the environments failed, see above",
            failed.len()
        ))
    }
}

fn setup_one(t3_home: &Path, data: &Path, source: &Source) -> Result<(), String> {
    let prefix = match source {
        Source::Local => "auth".to_owned(),
        Source::Remote { name, .. } => format!("auth {name}"),
    };
    // A remote tunnel stays open until the end, for the checks.
    let (api, _, _tunnel) = match source {
        Source::Local => {
            let address = server_address(t3_home).map_err(|error| error.to_string())?;
            let api = Api::new(address);
            let environment = api.environment().map_err(|error| error.to_string())?;
            (api, environment, None)
        }
        Source::Remote { .. } => reach(t3_home, source).map_err(|error| error.to_string())?,
    };
    let path = source.token_path(data);

    match load(&path).and_then(|token| verify(&api, &token)) {
        Ok(session) => {
            log!(
                "{prefix}: existing token works, expires {}",
                expiry(&session)
            );
            return Ok(());
        }
        Err(Failure::NoToken | Failure::Unauthorized | Failure::WrongScope) => {}
        Err(error) => return Err(format!("could not check the existing token: {error}")),
    }

    // Find storage problems before a token exists that could not be saved.
    let folder = path.parent().ok_or("no data folder")?;
    std::fs::create_dir_all(folder).map_err(|_| "cannot create the data folder")?;
    let staged = path.with_extension("new");
    let test = win::protect(b"self-test").ok_or("DPAPI is unavailable")?;
    std::fs::write(&staged, test).map_err(|_| "cannot write to the data folder")?;

    let token = match source {
        Source::Local => issue(t3_home)?,
        Source::Remote { remote, .. } => issue_remote(&remote.ssh)?,
    };
    let sealed = win::protect(token.0.as_bytes()).ok_or("DPAPI failed")?;
    std::fs::write(&staged, sealed)
        .and_then(|()| std::fs::rename(&staged, &path))
        .map_err(|_| "cannot save the token")?;

    let session = verify(&api, &token)
        .map_err(|error| format!("saved the new token, but checking it failed: {error}"))?;
    log!(
        "{prefix}: created an {SCOPE} token labeled {LABEL}, expires {}",
        expiry(&session)
    );
    Ok(())
}

fn issue_remote(target: &catalog::SshTarget) -> Result<Token, String> {
    let output = ssh::run(target, REMOTE_ISSUE_SCRIPT).map_err(|failure| match failure {
        Failure::Ssh("the remote command failed") => {
            "the remote T3 CLI could not issue a token".to_owned()
        }
        other => other.to_string(),
    })?;
    std::str::from_utf8(&output)
        .ok()
        .and_then(Token::parse)
        .ok_or_else(|| "the remote T3 CLI printed something other than a token".to_owned())
}

fn verify(api: &Api, token: &Token) -> Result<Session, Failure> {
    let session = api.session(token)?;
    check(&session)?;
    Ok(session)
}

fn expiry(session: &Session) -> &str {
    session
        .expires_at
        .as_deref()
        .unwrap_or("at an unknown time")
}

fn issue(t3_home: &Path) -> Result<Token, String> {
    // The scope is always passed: the CLI's default is nearly full access.
    let output = Command::new(t3_home.join("bin").join("t3.cmd"))
        .args([
            "auth", "session", "issue", "--scope", SCOPE, "--label", LABEL,
        ])
        .args(["--ttl", TTL, "--token-only"])
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|_| "cannot run t3.cmd")?;
    // Neither stream is echoed, whatever happened.
    if !output.status.success() {
        return Err(format!("t3.cmd exited with {:?}", output.status.code()));
    }
    std::str::from_utf8(&output.stdout)
        .ok()
        .and_then(Token::parse)
        .ok_or_else(|| "t3.cmd printed something other than a token".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(authenticated: bool, scopes: &[&str]) -> Session {
        Session {
            authenticated,
            scopes: Some(scopes.iter().map(|scope| (*scope).to_owned()).collect()),
            expires_at: None,
        }
    }

    #[test]
    fn only_the_read_scope_is_accepted() {
        assert!(check(&session(true, &[SCOPE])).is_ok());
        assert!(check(&session(false, &[SCOPE])).is_err());
        assert!(check(&session(true, &[])).is_err());
        assert!(check(&session(true, &[SCOPE, "orchestration:operate"])).is_err());
    }

    #[test]
    fn token_must_be_one_word() {
        assert!(Token::parse("invented.token.value.1234\r\n").is_some());
        assert!(Token::parse("").is_none());
        assert!(Token::parse("warning: something\ninvented.token.value.1234").is_none());
    }
}
