//! Keeps a subscription to T3's shell stream.
//!
//! The socket URL holds a ticket, so socket errors are mapped to fixed text and never printed.

pub mod auth;
mod http;
pub mod wire;

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::watch;
use tokio::time::{Instant, interval_at, sleep, timeout};
use tokio_tungstenite::tungstenite::{self, Message, protocol::WebSocketConfig};

use crate::log::log;
use http::{Api, Capabilities, server_address};
use wire::{Applied, ParseError, REQUEST_ID, Shell, StreamError, Threads};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(20);
const HEARTBEAT: Duration = Duration::from_secs(30);
const MAX_MESSAGE: usize = 64 * 1024 * 1024;
const MAX_BACKOFF: Duration = Duration::from_secs(30);

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
    pub environment_id: String,
    pub threads: Arc<Threads>,
}

#[derive(Debug)]
pub enum Failure {
    NotRunning,
    NotLoopback,
    NoToken,
    Unauthorized,
    WrongScope,
    Unsupported,
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

pub async fn run(t3_home: PathBuf, data: PathBuf, views: watch::Sender<View>) {
    let mut delay = Duration::from_secs(1);
    let mut last_failure = String::new();
    loop {
        let mut was_live = false;
        let failure = match prepare(t3_home.clone(), data.clone()).await {
            Ok(prepared) => {
                let Err(failure) = stream(prepared, &views, &mut was_live).await;
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
            log!("t3: {text}, retrying");
            last_failure = text;
        }
        sleep(delay).await;
        delay = (delay * 2).min(MAX_BACKOFF);
    }
}

struct Prepared {
    environment_id: String,
    capabilities: Capabilities,
    url: String,
    expires_at: Option<String>,
}

/// Everything before the socket. Read again on each attempt, so restarts and new tokens are seen.
async fn prepare(t3_home: PathBuf, data: PathBuf) -> Result<Prepared, Failure> {
    let blocking = move || {
        let address = server_address(&t3_home)?;
        let api = Api::new(address);
        let environment = api.environment()?;
        if environment.orchestration_protocol_version != Some(wire::PROTOCOL) {
            return Err(Failure::Unsupported);
        }
        let token = auth::load(&data)?;
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
        })
    };
    tokio::task::spawn_blocking(blocking)
        .await
        .unwrap_or(Err(Failure::Stream("connection setup panicked")))
}

async fn stream(
    prepared: Prepared,
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
                                            log!("t3: updates are flowing");
                                        }
                                    }
                                    Applied::Snapshot => {
                                        changed = true;
                                        if !*live {
                                            *live = true;
                                            log!(
                                                "t3: live, {} threads, token expires {}",
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
                    });
                }
            }
        }
    }
}
