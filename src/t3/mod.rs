//! Keeps a subscription to the shell stream of every T3 environment the desktop client knows:
//! the local server, and each remote one it reaches over SSH.
//!
//! The socket URL holds a ticket, so socket errors are mapped to fixed text and never printed.

pub mod auth;
pub mod catalog;
mod http;
mod merge;
#[cfg(test)]
mod recovery_tests;
mod ssh;
pub mod wire;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::watch;
use tokio::time::{Instant, interval_at, sleep, timeout};
use tokio_tungstenite::tungstenite::{self, Message, protocol::WebSocketConfig};

use crate::log::log;
use catalog::Remote;
use http::{Api, Capabilities, Environment, server_address};
#[cfg(test)]
pub use merge::remote_key as merge_key_for_tests;
pub use merge::{run_all, split_key};
use ssh::Tunnel;
use wire::{Applied, ParseError, Projects, REQUEST_ID, Shell, StreamError, Threads};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(20);
const HEARTBEAT: Duration = Duration::from_secs(30);
const MAX_MESSAGE: usize = 64 * 1024 * 1024;
// Short, because the local server is on loopback and restarts in seconds.
const MAX_BACKOFF: Duration = Duration::from_secs(5);
// Each remote attempt costs SSH connections, so those back off further.
const MAX_REMOTE_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Connecting,
    Live,
    Offline,
    NeedsToken,
    Unsupported,
}

#[derive(Clone, Default)]
pub struct View {
    pub phase: Phase,
    /// The local environment. Remote threads carry theirs in their key, see `split_key`.
    pub environment_id: String,
    pub threads: Arc<Threads>,
    pub projects: Arc<Projects>,
    /// Filled in by the merge only.
    pub health: Health,
}

/// The state of each source, for the tray.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Health {
    pub local: Phase,
    pub remotes: Vec<Phase>,
}

/// Where a stream comes from.
#[derive(Clone)]
pub enum Source {
    Local,
    /// `name` is what the log calls it, such as "t3 remote 1". Labels and hosts are not logged.
    Remote {
        name: String,
        remote: Remote,
    },
}

impl Source {
    fn name(&self) -> &str {
        match self {
            Self::Local => "t3",
            Self::Remote { name, .. } => name,
        }
    }

    fn max_backoff(&self) -> Duration {
        match self {
            Self::Local => MAX_BACKOFF,
            Self::Remote { .. } => MAX_REMOTE_BACKOFF,
        }
    }

    pub fn token_path(&self, data: &Path) -> PathBuf {
        match self {
            Self::Local => data.join("token.dpapi"),
            // Catalog ids are checked to be plain, see `catalog::valid_environment_id`.
            Self::Remote { remote, .. } => data
                .join("remotes")
                .join(format!("{}.dpapi", remote.environment_id)),
        }
    }
}

#[derive(Debug)]
pub enum Failure {
    NotRunning,
    NotLoopback,
    NoToken,
    Unauthorized,
    WrongScope,
    WrongEnvironment,
    Unsupported,
    Ssh(&'static str),
    Http(u16),
    Json(ParseError),
    Shell(StreamError),
    Stream(&'static str),
}

impl Failure {
    fn phase(&self) -> Phase {
        match self {
            Self::NoToken | Self::Unauthorized | Self::WrongScope => Phase::NeedsToken,
            Self::Unsupported => Phase::Unsupported,
            _ => Phase::Offline,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning => f.write_str("T3 Code is not reachable"),
            Self::NotLoopback => f.write_str("T3's origin is not loopback, refusing"),
            Self::NoToken => f.write_str("no saved token, run `t3-dock auth`"),
            Self::Unauthorized => f.write_str("token rejected or expired, run `t3-dock auth`"),
            Self::WrongScope => f.write_str("token is not read-only, refusing to use it"),
            Self::WrongEnvironment => f.write_str("the server is not the environment T3 saved"),
            Self::Ssh(what) => f.write_str(what),
            Self::Unsupported => write!(f, "T3 does not speak protocol {}", wire::PROTOCOL),
            Self::Http(status) => write!(f, "T3 answered HTTP {status}"),
            Self::Json(error) => error.fmt(f),
            Self::Shell(error) => error.fmt(f),
            Self::Stream(what) => f.write_str(what),
        }
    }
}

impl From<ParseError> for Failure {
    fn from(error: ParseError) -> Self {
        Self::Json(error)
    }
}

impl From<StreamError> for Failure {
    fn from(error: StreamError) -> Self {
        Self::Shell(error)
    }
}

pub async fn run(t3_home: PathBuf, data: PathBuf, source: Source, views: watch::Sender<View>) {
    let mut delay = Duration::from_secs(1);
    let mut last_failure = String::new();
    let name = source.name().to_owned();
    loop {
        let mut was_live = false;
        let failure = match prepare(t3_home.clone(), data.clone(), source.clone()).await {
            Ok(prepared) => {
                let Err(failure) = stream(prepared, &name, &views, &mut was_live).await;
                failure
            }
            Err(failure) => failure,
        };
        if was_live {
            delay = Duration::from_secs(1);
            last_failure.clear();
        }
        views.send_modify(|view| view.phase = failure.phase());

        let text = failure.to_string();
        if text != last_failure {
            log!("{name}: {text}, retrying");
            last_failure = text;
        }
        sleep(delay).await;
        delay = (delay * 2).min(source.max_backoff());
    }
}

struct Prepared {
    environment_id: String,
    capabilities: Capabilities,
    url: String,
    expires_at: Option<String>,
    /// Held for as long as the stream runs.
    _tunnel: Option<Tunnel>,
}

/// Finds the server and, for a remote, forwards a local port to it.
fn reach(t3_home: &Path, source: &Source) -> Result<(Api, Environment, Option<Tunnel>), Failure> {
    let (address, tunnel) = match source {
        Source::Local => (server_address(t3_home)?, None),
        Source::Remote { remote, .. } => {
            let port = ssh::remote_port(&remote.ssh)?;
            let tunnel = Tunnel::open(&remote.ssh, port)?;
            (tunnel.address, Some(tunnel))
        }
    };
    let api = Api::new(address);
    let environment = api.environment()?;
    if environment.orchestration_protocol_version != Some(wire::PROTOCOL) {
        return Err(Failure::Unsupported);
    }
    if let Source::Remote { remote, .. } = source
        && environment.environment_id != remote.environment_id
    {
        return Err(Failure::WrongEnvironment);
    }
    Ok((api, environment, tunnel))
}

/// Everything before the socket. Read again on each attempt, so restarts and new tokens are seen.
async fn prepare(t3_home: PathBuf, data: PathBuf, source: Source) -> Result<Prepared, Failure> {
    let blocking = move || {
        let (api, environment, tunnel) = reach(&t3_home, &source)?;
        let address = api.address();
        let token = auth::load(&source.token_path(&data))?;
        let session = api.session(&token)?;
        auth::check(&session)?;
        let ticket = crate::focus::percent_encode(&api.ticket(&token)?);
        Ok(Prepared {
            environment_id: environment.environment_id,
            capabilities: environment.capabilities.unwrap_or_default(),
            url: format!(
                "ws://{address}/ws?orchestrationProtocol={}&wsTicket={ticket}",
                wire::PROTOCOL
            ),
            expires_at: session.expires_at,
            _tunnel: tunnel,
        })
    };
    tokio::task::spawn_blocking(blocking)
        .await
        .unwrap_or(Err(Failure::Stream("connection setup panicked")))
}

async fn stream(
    prepared: Prepared,
    name: &str,
    views: &watch::Sender<View>,
    live: &mut bool,
) -> Result<std::convert::Infallible, Failure> {
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    let connecting =
        tokio_tungstenite::connect_async_with_config(&prepared.url, Some(config), true);
    let (mut socket, _) = timeout(CONNECT_TIMEOUT, connecting)
        .await
        .map_err(|_| Failure::Stream("socket connection timed out"))?
        .map_err(|error| match error {
            tungstenite::Error::Http(response) => Failure::Http(response.status().as_u16()),
            _ => Failure::Stream("socket connection failed"),
        })?;

    let mut payload = serde_json::Map::new();
    if prepared.capabilities.shell_thread_families == Some(true) {
        payload.insert("omitIdleSubagents".into(), true.into());
    }
    if prepared.capabilities.shell_older_settled == Some(true) {
        payload.insert("omitOlderSettled".into(), true.into());
    }
    let request = json!({
        "_tag": "Request",
        "id": REQUEST_ID,
        "tag": "orchestration.subscribeShell",
        "payload": payload,
        "headers": [],
    });
    let ack = json!({"_tag": "Ack", "requestId": REQUEST_ID}).to_string();
    let ping = json!({"_tag": "Ping"}).to_string();
    let pong = json!({"_tag": "Pong"}).to_string();
    let closed = |_| Failure::Stream("socket write failed");

    socket
        .send(Message::text(request.to_string()))
        .await
        .map_err(closed)?;

    let snapshot_deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    let mut heartbeat = interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
    let mut answered = true;
    let mut shell = Shell::default();
    let mut updated = false;

    loop {
        tokio::select! {
            () = tokio::time::sleep_until(snapshot_deadline), if !*live => {
                return Err(Failure::Stream("no snapshot arrived"));
            }
            _ = heartbeat.tick() => {
                if !answered {
                    return Err(Failure::Stream("heartbeat went unanswered"));
                }
                answered = false;
                socket.send(Message::text(ping.clone())).await.map_err(closed)?;
            }
            message = socket.next() => {
                let message = message
                    .ok_or(Failure::Stream("socket closed"))?
                    .map_err(|_| Failure::Stream("socket read failed"))?;
                let text = match message {
                    Message::Text(text) => text,
                    Message::Close(_) => return Err(Failure::Stream("server closed the socket")),
                    _ => continue,
                };

                let mut changed = false;
                for frame in wire::parse_frames(&text)? {
                    match frame.tag.as_str() {
                        "Pong" => answered = true,
                        "Ping" => socket.send(Message::text(pong.clone())).await.map_err(closed)?,
                        "Chunk" if frame.request_id.as_deref() == Some(REQUEST_ID) => {
                            for item in frame.values.unwrap_or_default() {
                                match shell.apply(item)? {
                                    Applied::Unchanged => {}
                                    Applied::Changed => {
                                        changed = true;
                                        if !updated {
                                            updated = true;
                                            log!("{name}: updates are flowing");
                                        }
                                    }
                                    Applied::Snapshot => {
                                        changed = true;
                                        if !*live {
                                            *live = true;
                                            log!(
                                                "{name}: live, {} threads, token expires {}",
                                                shell.threads.len(),
                                                prepared.expires_at.as_deref().unwrap_or("unknown")
                                            );
                                        }
                                    }
                                }
                            }
                            // The server sends nothing more until this arrives.
                            socket.send(Message::text(ack.clone())).await.map_err(closed)?;
                        }
                        "Exit" => return Err(Failure::Stream("subscription ended")),
                        "Defect" | "ClientProtocolError" => {
                            return Err(Failure::Stream("server reported a protocol error"));
                        }
                        _ => {}
                    }
                }
                if changed {
                    views.send_replace(View {
                        phase: Phase::Live,
                        environment_id: prepared.environment_id.clone(),
                        threads: Arc::new(shell.threads.clone()),
                        projects: Arc::new(shell.projects.clone()),
                        health: Health::default(),
                    });
                }
            }
        }
    }
}
