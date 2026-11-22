//! The read-only T3 token: issued by T3's CLI, kept with DPAPI, shown to nobody.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::Failure;
use super::http::{Api, Session, server_address};
use crate::log::log;
use crate::win;

const SCOPE: &str = "orchestration:read";
const LABEL: &str = "t3-dock";
const TTL: &str = "30d";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

fn token_path(data: &Path) -> PathBuf {
    data.join("token.dpapi")
}

pub fn load(data: &Path) -> Result<Token, Failure> {
    let sealed = std::fs::read(token_path(data)).map_err(|_| Failure::NoToken)?;
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

/// `t3-dock auth`: keeps a working token, otherwise issues one.
pub fn setup(t3_home: &Path, data: &Path) -> Result<(), String> {
    let address = server_address(t3_home).map_err(|error| error.to_string())?;
    let api = Api::new(address);
    api.environment().map_err(|error| error.to_string())?;

    match load(data).and_then(|token| verify(&api, &token)) {
        Ok(session) => {
            log!("auth: existing token works, expires {}", expiry(&session));
            return Ok(());
        }
        Err(Failure::NoToken | Failure::Unauthorized | Failure::WrongScope) => {}
        Err(error) => return Err(format!("could not check the existing token: {error}")),
    }

    // Find storage problems before a token exists that could not be saved.
    std::fs::create_dir_all(data).map_err(|_| "cannot create the data folder")?;
    let staged = token_path(data).with_extension("new");
    let test = win::protect(b"self-test").ok_or("DPAPI is unavailable")?;
    std::fs::write(&staged, test).map_err(|_| "cannot write to the data folder")?;

    let token = issue(t3_home)?;
    let sealed = win::protect(token.0.as_bytes()).ok_or("DPAPI failed")?;
    std::fs::write(&staged, sealed)
        .and_then(|()| std::fs::rename(&staged, token_path(data)))
        .map_err(|_| "cannot save the token")?;

    let session = verify(&api, &token)
        .map_err(|error| format!("saved the new token, but checking it failed: {error}"))?;
    log!(
        "auth: created an {SCOPE} token labeled {LABEL}, expires {}",
        expiry(&session)
    );
    Ok(())
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
