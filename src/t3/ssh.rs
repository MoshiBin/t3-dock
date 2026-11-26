//! Reaches a remote T3 server the way the desktop client does: over an SSH port forward to the
//! server's loopback port. Nothing is ever launched on the remote. The desktop client owns the
//! remote server, and t3-dock only joins one that is already running.
//!
//! `ssh.exe` always runs with `BatchMode=yes`, so it never prompts. Keys or an agent must work,
//! as they do for the desktop client. Its output is never logged: the token is printed on it.

use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use zeroize::Zeroizing;

use super::Failure;
use super::catalog::SshTarget;
use super::http::loopback;
use crate::win;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const TUNNEL_TIMEOUT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(100);
const OUTPUT_LIMIT: u64 = 64 * 1024;

/// The desktop client starts the remote server with T3's default home, so its runtime file is here.
const RUNTIME_SCRIPT: &str = r#"cat "$HOME/.t3/userdata/server-runtime.json""#;

fn destination(target: &SshTarget) -> String {
    let host = if target.alias.trim().is_empty() {
        target.hostname.trim()
    } else {
        target.alias.trim()
    };
    match &target.username {
        Some(user) => format!("{user}@{host}"),
        None => host.to_owned(),
    }
}

/// The desktop client's options, minus anything interactive.
fn base_args(target: &SshTarget) -> Vec<String> {
    let mut args: Vec<String> = [
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "RemoteCommand=none",
        "-o",
        "RequestTTY=no",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
    ]
    .map(String::from)
    .into();
    if let Some(port) = target.port {
        args.extend(["-p".into(), port.to_string()]);
    }
    args
}

fn ssh() -> Command {
    let mut command = Command::new("ssh.exe");
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    command
}

/// Runs one shell command on the remote and returns its standard output.
pub fn run(target: &SshTarget, script: &str) -> Result<Zeroizing<Vec<u8>>, Failure> {
    let mut child = ssh()
        .args(base_args(target))
        .arg("--")
        .arg(destination(target))
        .arg(script)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| Failure::Ssh("cannot run ssh.exe"))?;
    win::kill_with_this_process(&child);
    let mut stdout = child
        .stdout
        .take()
        .ok_or(Failure::Ssh("ssh.exe has no output"))?;
    let reader = std::thread::spawn(move || {
        let mut output = Zeroizing::new(Vec::new());
        let _ = (&mut stdout).take(OUTPUT_LIMIT).read_to_end(&mut output);
        output
    });

    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Failure::Ssh("the SSH command timed out"));
            }
        }
    };
    let output = reader
        .join()
        .map_err(|_| Failure::Ssh("reading SSH output failed"))?;
    match status.code() {
        Some(0) => Ok(output),
        // ssh.exe's own failures: unreachable host, refused key, unknown host key.
        Some(255) => Err(Failure::Ssh("SSH could not connect without a prompt")),
        _ => Err(Failure::Ssh("the remote command failed")),
    }
}

/// The port of the T3 server running on the remote, from its runtime file.
pub fn remote_port(target: &SshTarget) -> Result<u16, Failure> {
    #[derive(Deserialize)]
    struct Runtime {
        origin: String,
    }
    let output = run(target, RUNTIME_SCRIPT).map_err(|failure| match failure {
        Failure::Ssh("the remote command failed") => Failure::NotRunning,
        other => other,
    })?;
    let runtime: Runtime = serde_json::from_slice(&output)
        .map_err(|_| Failure::Stream("remote runtime file is unreadable"))?;
    // The forward targets the remote's 127.0.0.1, so the server must listen there.
    loopback(&runtime.origin)
        .map(|address| address.port())
        .ok_or(Failure::NotLoopback)
}

/// A local port forwarded to the remote server. Dropping it ends `ssh.exe`.
pub struct Tunnel {
    child: Child,
    pub address: SocketAddr,
}

impl Tunnel {
    pub fn open(target: &SshTarget, remote_port: u16) -> Result<Self, Failure> {
        // The port is free when picked. Should something take it first, ssh exits and we retry.
        let local_port = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .map_err(|_| Failure::Ssh("no free local port"))?
            .port();
        let address = SocketAddr::from(([127, 0, 0, 1], local_port));
        let child = ssh()
            .args(base_args(target))
            .args(["-o", "ExitOnForwardFailure=yes", "-o", "ControlMaster=no"])
            .args(["-o", "ControlPath=none", "-n", "-N", "-L"])
            .arg(format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"))
            .arg("--")
            .arg(destination(target))
            .stdout(Stdio::null())
            .spawn()
            .map_err(|_| Failure::Ssh("cannot run ssh.exe"))?;
        win::kill_with_this_process(&child);
        let mut tunnel = Self { child, address };

        let deadline = Instant::now() + TUNNEL_TIMEOUT;
        loop {
            if !matches!(tunnel.child.try_wait(), Ok(None)) {
                return Err(Failure::Ssh("SSH could not open the forward"));
            }
            if TcpStream::connect_timeout(&address, POLL).is_ok() {
                return Ok(tunnel);
            }
            if Instant::now() >= deadline {
                return Err(Failure::Ssh("the SSH forward did not come up"));
            }
            std::thread::sleep(POLL);
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(alias: &str, user: Option<&str>, port: Option<u16>) -> SshTarget {
        SshTarget {
            alias: alias.into(),
            hostname: "host.invalid".into(),
            username: user.map(Into::into),
            port,
        }
    }

    #[test]
    fn the_alias_wins_so_ssh_config_applies() {
        assert_eq!(destination(&target("box", Some("me"), None)), "me@box");
        assert_eq!(destination(&target(" ", None, None)), "host.invalid");
    }

    #[test]
    fn ssh_never_prompts() {
        let args = base_args(&target("box", None, Some(2222)));
        assert!(args.windows(2).any(|pair| pair == ["-o", "BatchMode=yes"]));
        assert!(args.windows(2).any(|pair| pair == ["-p", "2222"]));
    }
}
