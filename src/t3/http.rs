//! T3's loopback HTTP endpoints. No proxies, no redirects, and no response bodies in errors.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::Failure;
use super::auth::Token;
use super::wire::ParseError;

const BODY_LIMIT: u64 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub environment_id: String,
    pub orchestration_protocol_version: Option<u64>,
    pub capabilities: Option<Capabilities>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub shell_thread_families: Option<bool>,
    pub shell_older_settled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub authenticated: bool,
    pub scopes: Option<Vec<String>>,
    pub expires_at: Option<String>,
}

#[derive(Deserialize)]
struct Ticket {
    ticket: String,
}

/// Reads T3's origin from its runtime file.
pub fn server_address(t3_home: &Path) -> Result<SocketAddr, Failure> {
    #[derive(Deserialize)]
    struct Runtime {
        origin: String,
    }
    let path = t3_home.join("userdata").join("server-runtime.json");
    let text = std::fs::read_to_string(path).map_err(|_| Failure::NotRunning)?;
    let runtime: Runtime = serde_json::from_str(&text).map_err(ParseError::from)?;
    loopback(&runtime.origin).ok_or(Failure::NotLoopback)
}

/// Accepts only plain HTTP to a loopback address. Parsing to a socket address, not a URL, leaves
/// no room for credentials, paths or host names that resolve elsewhere.
pub(super) fn loopback(origin: &str) -> Option<SocketAddr> {
    let authority = origin.strip_prefix("http://")?;
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    let address: SocketAddr = match authority.strip_prefix("localhost:") {
        Some(port) => SocketAddr::from(([127, 0, 0, 1], port.parse().ok()?)),
        None => authority.parse().ok()?,
    };
    address.ip().is_loopback().then_some(address)
}

pub struct Api {
    agent: ureq::Agent,
    address: SocketAddr,
}

impl Api {
    pub fn new(address: SocketAddr) -> Self {
        let agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        Self { agent, address }
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    pub fn environment(&self) -> Result<Environment, Failure> {
        read(
            self.agent
                .get(self.url("/.well-known/t3/environment"))
                .call(),
        )
    }

    pub fn session(&self, token: &Token) -> Result<Session, Failure> {
        read(
            self.agent
                .get(self.url("/api/auth/session"))
                .header("authorization", token.bearer())
                .call(),
        )
    }

    pub fn ticket(&self, token: &Token) -> Result<String, Failure> {
        let ticket: Ticket = read(
            self.agent
                .post(self.url("/api/auth/websocket-ticket"))
                .header("authorization", token.bearer())
                .header("content-type", "application/json")
                .send("{}"),
        )?;
        Ok(ticket.ticket)
    }
}

fn read<T: DeserializeOwned>(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<T, Failure> {
    let mut response = response.map_err(|_| Failure::NotRunning)?;
    match response.status().as_u16() {
        200..=299 => {}
        401 | 403 => return Err(Failure::Unauthorized),
        status => return Err(Failure::Http(status)),
    }
    let text = response
        .body_mut()
        .with_config()
        .limit(BODY_LIMIT)
        .read_to_string()
        .map_err(|_| Failure::Stream("HTTP response could not be read"))?;
    Ok(serde_json::from_str(&text).map_err(ParseError::from)?)
}

#[cfg(test)]
mod tests {
    use super::loopback;

    #[test]
    fn accepts_loopback() {
        for origin in [
            "http://127.0.0.1:3773",
            "http://127.0.0.1:3773/",
            "http://localhost:3773",
            "http://[::1]:3773",
        ] {
            assert!(loopback(origin).is_some(), "{origin}");
        }
    }

    #[test]
    fn refuses_everything_else() {
        for origin in [
            "http://0.0.0.0:3773",
            "http://192.168.1.20:3773",
            "http://example.com:3773",
            "http://127.0.0.1:3773@example.com",
            "http://localhost:3773@example.com",
            "http://localhost.example.com:3773",
            "http://127.0.0.1:3773/path",
            "https://127.0.0.1:3773",
            "127.0.0.1:3773",
        ] {
            assert!(loopback(origin).is_none(), "{origin}");
        }
    }
}
