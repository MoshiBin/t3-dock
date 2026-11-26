//! Optional: the user's Devin ACU allowance, for a tile on the side display.
//!
//! Signing in follows the Devin CLI's browser login (`devin auth login`): PKCE through the
//! Devin web app, a redirect to a one-shot listener on 127.0.0.1, and an exchange of the code at
//! the Devin API for a session token. None of this is a documented API; see docs/HANDOFF.md.
//!
//! The token can do anything the user's Devin account can, so it is kept with DPAPI, sent only
//! over HTTPS to the Devin hosts, and used only to read the allowance. Neither it, the code, nor
//! the user's Devin address is logged.

mod proto;

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::log::log;
use crate::win;

pub const TOKEN_FILE: &str = "devin.token";
pub const DEFAULT_ADDRESS: &str = "app.devin.ai";
/// Where the allowance is read, for every Devin deployment.
const STATUS_URL: &str =
    "https://server.codeium.com/exa.seat_management_pb.SeatManagementService/GetUserStatus";
const SIGN_IN_TIME: Duration = Duration::from_secs(300);
const SESSION_PREFIX: &str = "devin-session-token$";

/// What is left of the allowance this billing cycle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Allowance {
    pub used: f64,
    pub limit: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// Devin refused the token: sign in again.
    Rejected,
    /// The account has no ACU limit to show.
    NoAllowance,
    Unreachable,
}

/// The sealed file. Implements neither `Debug` nor `Display`.
#[derive(Serialize, Deserialize)]
struct Stored {
    address: String,
    token: String,
}

/// A signed-in account. Implements neither `Debug` nor `Display`.
pub struct Account {
    address: String,
    token: Zeroizing<String>,
}

impl Account {
    /// The Devin web app it signed in to, for the settings window only.
    pub fn address(&self) -> &str {
        &self.address
    }
}

pub fn token_path(data: &Path) -> PathBuf {
    data.join(TOKEN_FILE)
}

pub fn load(data: &Path) -> Option<Account> {
    let sealed = std::fs::read(token_path(data)).ok()?;
    let plain = Zeroizing::new(win::unprotect(&sealed)?);
    let stored: Stored = serde_json::from_slice(&plain).ok()?;
    let token = Zeroizing::new(stored.token);
    valid_token(&token).then_some(Account {
        address: stored.address,
        token,
    })
}

pub fn sign_out(data: &Path) -> bool {
    match std::fs::remove_file(token_path(data)) {
        Ok(()) => {
            log!("devin: signed out");
            true
        }
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

fn valid_token(token: &str) -> bool {
    (16..=8192).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic())
}

/// A host name typed by the user, with or without `https://` and a trailing slash.
pub fn normalize_address(text: &str) -> Option<String> {
    let text = text.trim().to_ascii_lowercase();
    let host = text.strip_prefix("https://").unwrap_or(&text);
    let host = host.strip_suffix('/').unwrap_or(host);
    let labels: Vec<&str> = host.split('.').collect();
    let valid = labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    valid.then(|| host.to_owned())
}

/// The API host of a web app: `app.devin.ai` has `api.devin.ai`, and an enterprise deployment
/// `<name>.devinenterprise.com` has `api.devinenterprise.com`.
fn api_host(address: &str) -> String {
    match address.split_once('.') {
        Some((_, domain)) => format!("api.{domain}"),
        None => address.to_owned(),
    }
}

fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .https_only(true)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into()
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(at + 1..at + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                at += 3;
            }
            b'+' => {
                out.push(b' ');
                at += 1;
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn query_value(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| percent_decode(value)).flatten()
    })
}

/// Signs in through the browser and saves the token. Blocks until done, failed or `cancel`.
pub fn sign_in(address: &str, data: &Path, cancel: &Arc<AtomicBool>) -> Result<(), String> {
    let address = normalize_address(address).ok_or("That is not a Devin address.")?;
    let random = || win::random_bytes::<32>().ok_or("No random numbers are available.");
    let verifier = Zeroizing::new(URL_SAFE_NO_PAD.encode(random()?));
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = URL_SAFE_NO_PAD.encode(&random()?[..16]);

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|_| "Cannot listen for the browser.")?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "Cannot listen for the browser.")?;
    let port = listener
        .local_addr()
        .map_err(|_| "Cannot listen for the browser.")?
        .port();
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let url = format!(
        "https://{address}/auth/cli/continue?redirect_uri={}&state={state}\
         &prompt=select_account&code_challenge={challenge}&code_challenge_method=S256",
        percent_encode(&redirect)
    );
    if !win::shell_open(&url) {
        return Err("Cannot open the browser.".into());
    }
    log!("devin: waiting for the browser");

    let code = wait_for_code(&listener, &state, cancel)?;
    let token = exchange(&address, &code, &verifier)?;
    let account = Account { address, token };
    match allowance(&account) {
        Ok(_) | Err(Failure::NoAllowance) => {}
        Err(Failure::Rejected) => return Err("Devin did not accept the sign-in.".into()),
        Err(Failure::Unreachable) => return Err("Signed in, but Devin is not reachable.".into()),
    }
    save(data, &account)?;
    log!("devin: signed in");
    Ok(())
}

fn wait_for_code(
    listener: &TcpListener,
    state: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<Zeroizing<String>, String> {
    let deadline = Instant::now() + SIGN_IN_TIME;
    while Instant::now() < deadline {
        if cancel.load(Ordering::Relaxed) {
            return Err("Sign-in canceled.".into());
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(code) = answer(stream, state) {
                    return Ok(code);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return Err("Cannot listen for the browser.".into()),
        }
    }
    Err("The browser did not finish signing in within 5 minutes.".into())
}

/// Answers one browser request. The code, if this was the callback with our state.
fn answer(mut stream: TcpStream, state: &str) -> Option<Zeroizing<String>> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut reader = BufReader::new(&stream);
    let mut line = Zeroizing::new(String::new());
    if !reader.read_line(&mut line).is_ok_and(|read| read > 0) {
        return None;
    }
    // Read the headers too, so closing the connection doesn't reset it under the browser.
    let mut header = String::new();
    for _ in 0..100 {
        header.clear();
        if !reader.read_line(&mut header).is_ok_and(|read| read > 2) {
            break;
        }
    }
    let target = line.split(' ').nth(1).unwrap_or_default();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let (status, body, code) = if path != "/callback" {
        ("404 Not Found", "Not found.", None)
    } else if query_value(query, "state").as_deref() != Some(state) {
        (
            "400 Bad Request",
            "This sign-in is not the one t3-dock started.",
            None,
        )
    } else {
        match query_value(query, "code") {
            Some(code) if !code.is_empty() => (
                "200 OK",
                "t3-dock is signed in to Devin. You can close this tab.",
                Some(Zeroizing::new(code)),
            ),
            _ => ("400 Bad Request", "Devin sent no sign-in code.", None),
        }
    };
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>t3-dock</title>\
         <body style=\"font-family:sans-serif;margin:3em\"><h2>{body}</h2></body>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\n\
         content-length: {}\r\ncache-control: no-store\r\nconnection: close\r\n\r\n{page}",
        page.len()
    );
    code
}

#[derive(Deserialize)]
struct Exchanged {
    token: String,
}

fn exchange(address: &str, code: &str, verifier: &str) -> Result<Zeroizing<String>, String> {
    let body =
        Zeroizing::new(serde_json::json!({"code": code, "code_verifier": verifier}).to_string());
    let url = format!("https://{}/auth/cli/token", api_host(address));
    let mut response = agent()
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .send(body.as_str())
        .map_err(|_| "Devin is not reachable.")?;
    let status = response.status().as_u16();
    if status != 200 {
        log!("devin: the sign-in exchange returned HTTP {status}");
        return Err(format!("Devin refused the sign-in (HTTP {status})."));
    }
    let text = Zeroizing::new(
        response
            .body_mut()
            .read_to_string()
            .map_err(|_| "Devin sent an unreadable answer.")?,
    );
    let exchanged: Exchanged =
        serde_json::from_str(&text).map_err(|_| "Devin sent an unexpected answer.")?;
    let token = Zeroizing::new(exchanged.token);
    if !valid_token(&token) {
        return Err("Devin sent an unexpected answer.".into());
    }
    Ok(token)
}

fn save(data: &Path, account: &Account) -> Result<(), String> {
    let stored = Zeroizing::new(
        serde_json::to_vec(&Stored {
            address: account.address.clone(),
            token: account.token.to_string(),
        })
        .map_err(|_| "Cannot save the sign-in.")?,
    );
    let sealed = win::protect(&stored).ok_or("Cannot encrypt the sign-in.")?;
    std::fs::create_dir_all(data).map_err(|_| "Cannot create the data folder.")?;
    let path = token_path(data);
    let staged = path.with_extension("new");
    std::fs::write(&staged, sealed)
        .and_then(|()| std::fs::rename(&staged, &path))
        .map_err(|_| "Cannot save the sign-in.".to_owned())
}

/// The request: `GetUserStatusRequest { metadata }`, naming t3-dock as the client.
fn status_request(token: &str) -> Zeroizing<Vec<u8>> {
    let api_key = Zeroizing::new(if token.starts_with(SESSION_PREFIX) {
        token.to_owned()
    } else {
        format!("{SESSION_PREFIX}{token}")
    });
    let version = env!("CARGO_PKG_VERSION");
    let mut metadata = Zeroizing::new(Vec::new());
    for (number, value) in [
        (1, "t3-dock"),
        (7, version),
        (12, "t3-dock"),
        (2, version),
        (3, api_key.as_str()),
        (4, "en"),
        (5, "windows"),
    ] {
        proto::bytes_field(number, value.as_bytes(), &mut metadata);
    }
    let mut request = Zeroizing::new(Vec::new());
    proto::bytes_field(1, &metadata, &mut request);
    request
}

/// `GetUserStatusResponse.user_status.plan_status`: ACUs consumed (19) and the limit (20).
fn parse_allowance(response: &[u8]) -> Result<Allowance, Failure> {
    let plan = proto::fields(response)
        .and_then(|top| proto::message(&top, 1))
        .and_then(|user| proto::message(&user, 13))
        .ok_or(Failure::Unreachable)?;
    let used = proto::double(&plan, 19).ok_or(Failure::Unreachable)?;
    let limit = proto::double(&plan, 20).ok_or(Failure::Unreachable)?;
    if !(limit.is_finite() && limit > 0.0 && used.is_finite()) {
        return Err(Failure::NoAllowance);
    }
    Ok(Allowance {
        used: used.max(0.0),
        limit,
    })
}

pub fn allowance(account: &Account) -> Result<Allowance, Failure> {
    let request = status_request(&account.token);
    let mut response = agent()
        .post(STATUS_URL)
        .header("content-type", "application/proto")
        .header("connect-protocol-version", "1")
        .header("accept-encoding", "identity")
        .send(request.as_slice())
        .map_err(|_| Failure::Unreachable)?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .with_config()
        .limit(1 << 20)
        .read_to_vec()
        .map_err(|_| Failure::Unreachable)?;
    match status {
        200 => parse_allowance(&body),
        401 | 403 => Err(Failure::Rejected),
        _ => {
            // Connect errors are JSON with a code, such as "unauthenticated".
            let code = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value.get("code")?.as_str().map(str::to_owned));
            match code.as_deref() {
                Some("unauthenticated" | "permission_denied") => Err(Failure::Rejected),
                _ => Err(Failure::Unreachable),
            }
        }
    }
}

/// What the dock knows of the allowance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    SignedOut,
    Loading,
    Known(Allowance),
    Rejected,
    NoAllowance,
    /// With the last allowance read, if any.
    Unreachable(Option<Allowance>),
}

/// Reads the allowance every few minutes while signed in, and at once after a new sign-in.
pub async fn poll(data: PathBuf, status: tokio::sync::watch::Sender<Status>) {
    const REFRESH: Duration = Duration::from_secs(120);
    const AFTER_REJECTION: Duration = Duration::from_secs(1800);
    let path = token_path(&data);
    let (mut seen, mut next, mut last) = (None, Instant::now(), None);
    loop {
        let modified = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok();
        if modified != seen {
            seen = modified;
            next = Instant::now();
            last = None;
            status.send_replace(if modified.is_some() {
                Status::Loading
            } else {
                Status::SignedOut
            });
        }
        if modified.is_some() && Instant::now() >= next {
            let folder = data.clone();
            let read = tokio::task::spawn_blocking(move || {
                load(&folder).map(|account| allowance(&account))
            })
            .await;
            let now = match read {
                Ok(None) => Status::SignedOut,
                Ok(Some(Ok(allowance))) => {
                    last = Some(allowance);
                    Status::Known(allowance)
                }
                Ok(Some(Err(Failure::Rejected))) => Status::Rejected,
                Ok(Some(Err(Failure::NoAllowance))) => Status::NoAllowance,
                Ok(Some(Err(Failure::Unreachable))) | Err(_) => Status::Unreachable(last),
            };
            let before = *status.borrow();
            if std::mem::discriminant(&before) != std::mem::discriminant(&now) {
                log!("devin: {}", describe(&now));
            }
            next = Instant::now()
                + if now == Status::Rejected {
                    AFTER_REJECTION
                } else {
                    REFRESH
                };
            status.send_replace(now);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// For the log: no numbers, which are the user's.
fn describe(status: &Status) -> &'static str {
    match status {
        Status::SignedOut => "not signed in",
        Status::Loading => "loading",
        Status::Known(_) => "allowance read",
        Status::Rejected => "the sign-in was rejected",
        Status::NoAllowance => "the account has no ACU limit",
        Status::Unreachable(_) => "not reachable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_host_names() {
        assert_eq!(
            normalize_address(" https://App.Devin.ai/ ").as_deref(),
            Some("app.devin.ai")
        );
        assert_eq!(
            normalize_address("acme.devinenterprise.com").as_deref(),
            Some("acme.devinenterprise.com")
        );
        for bad in [
            "",
            "devin",
            "http://app.devin.ai",
            "app.devin.ai/path",
            "a..b",
            "x y.com",
        ] {
            assert_eq!(normalize_address(bad), None, "{bad}");
        }
        assert_eq!(api_host("app.devin.ai"), "api.devin.ai");
        assert_eq!(
            api_host("acme.devinenterprise.com"),
            "api.devinenterprise.com"
        );
    }

    #[test]
    fn queries_are_decoded() {
        let query = "code=a%2Bb%3D&state=x+y&empty=";
        assert_eq!(query_value(query, "code").as_deref(), Some("a+b="));
        assert_eq!(query_value(query, "state").as_deref(), Some("x y"));
        assert_eq!(query_value(query, "missing"), None);
        assert_eq!(
            percent_encode("http://127.0.0.1:5/callback"),
            "http%3A%2F%2F127.0.0.1%3A5%2Fcallback"
        );
    }

    #[test]
    fn the_request_names_t3_dock_and_carries_the_session() {
        let request = status_request("abc.def.ghi");
        let top = proto::fields(&request).unwrap();
        let metadata = proto::message(&top, 1).unwrap();
        assert_eq!(
            proto::find(&metadata, 1),
            Some(proto::Value::Bytes(b"t3-dock"))
        );
        let key = format!("{SESSION_PREFIX}abc.def.ghi");
        assert_eq!(
            proto::find(&metadata, 3),
            Some(proto::Value::Bytes(key.as_bytes()))
        );
    }

    fn response(used: f64, limit: Option<f64>) -> Vec<u8> {
        let mut plan = Vec::new();
        proto::varint(19 << 3 | 1, &mut plan);
        plan.extend_from_slice(&used.to_le_bytes());
        if let Some(limit) = limit {
            proto::varint(20 << 3 | 1, &mut plan);
            plan.extend_from_slice(&limit.to_le_bytes());
        }
        let mut user = Vec::new();
        proto::bytes_field(3, b"Invented Name", &mut user);
        proto::bytes_field(13, &plan, &mut user);
        let mut top = Vec::new();
        proto::bytes_field(1, &user, &mut top);
        top
    }

    #[test]
    fn the_allowance_is_read_from_the_plan() {
        assert_eq!(
            parse_allowance(&response(1270.5, Some(1350.0))),
            Ok(Allowance {
                used: 1270.5,
                limit: 1350.0
            })
        );
        assert_eq!(
            parse_allowance(&response(3.0, None)),
            Err(Failure::NoAllowance)
        );
        assert_eq!(parse_allowance(b"\xff"), Err(Failure::Unreachable));
    }

    #[test]
    fn the_callback_must_carry_our_state() {
        use std::io::Read;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let send = |path: &'static str| {
            std::thread::spawn(move || {
                let mut stream = TcpStream::connect(address).unwrap();
                write!(stream, "GET {path} HTTP/1.1\r\nhost: x\r\n\r\n").unwrap();
                let mut reply = String::new();
                stream.read_to_string(&mut reply).unwrap();
                reply
            })
        };
        let cases = [
            ("/callback?code=c1&state=wrong", None, "400"),
            ("/favicon.ico", None, "404"),
            ("/callback?code=c%2F2&state=s1", Some("c/2"), "200"),
        ];
        for (path, code, status) in cases {
            let client = send(path);
            let (stream, _) = listener.accept().unwrap();
            assert_eq!(answer(stream, "s1").as_deref().map(String::as_str), code);
            assert!(
                client
                    .join()
                    .unwrap()
                    .starts_with(&format!("HTTP/1.1 {status}"))
            );
        }
    }

    /// Signs in for real and reads the allowance: needs a browser that can sign in to Devin.
    /// `T3_DOCK_DEVIN_ADDRESS=<address> cargo test devin_sign_in -- --ignored --nocapture`
    #[test]
    #[ignore = "signs in to Devin and saves the sign-in"]
    fn devin_sign_in() {
        let address = std::env::var("T3_DOCK_DEVIN_ADDRESS").unwrap();
        let data = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap()).join("t3-dock");
        sign_in(&address, &data, &Arc::new(AtomicBool::new(false))).unwrap();
        let account = load(&data).unwrap();
        let allowance = allowance(&account).unwrap();
        assert!(allowance.limit > 0.0);
        println!(
            "used {:.0}% of the allowance",
            allowance.used / allowance.limit * 100.0
        );
    }
}
