mod app;
mod board;
mod device;
mod focus;
mod input;
mod log;
mod render;
mod status;
mod t3;
mod win;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use jiff::Timestamp;
use tokio::sync::{mpsc, watch};
use tokio::time::{interval, timeout};

use crate::app::App;
use crate::log::log;

const USAGE: &str = "usage:
  t3-dock [run]    show threads on the N3
  t3-dock auth     create or check the read-only T3 token
  t3-dock stop     stop the running instance";

const RESUME_GAP: Duration = Duration::from_secs(60);

enum Cli {
    Run,
    Auth,
    Stop,
}

fn parse(args: &[&str]) -> Option<Cli> {
    match args {
        [] | ["run"] => Some(Cli::Run),
        ["auth"] => Some(Cli::Auth),
        ["stop"] => Some(Cli::Stop),
        _ => None,
    }
}

/// The deep link starts a second T3 process with our environment. Started from a terminal or
/// agent inside T3, that would include variables that change how Electron and T3 start.
fn clean_environment() {
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(is_session_variable) {
            // SAFETY: main calls this before it starts any other thread.
            unsafe { std::env::remove_var(&key) };
        }
    }
}

/// Variables that describe the session we were started from. The user's own settings, such as
/// `T3CODE_HOME` or `CLAUDE_CONFIG_DIR`, are kept.
fn is_session_variable(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    (name.starts_with("T3CODE_") && name != "T3CODE_HOME")
        || name.starts_with("ELECTRON_")
        || name.starts_with("CLAUDE_CODE_SESSION_")
        || name.starts_with("CLAUDE_CODE_MESSAGING_")
        || name.starts_with("CLAUDE_CODE_CHILD_")
        || matches!(
            name.as_str(),
            "CLAUDECODE"
                | "CLAUDE_CODE_ENTRYPOINT"
                | "CLAUDE_CODE_EXECPATH"
                | "CLAUDE_AGENT_SDK_VERSION"
                | "CLAUDE_EFFORT"
                | "CLAUDE_PID"
        )
}

fn main() -> ExitCode {
    clean_environment();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(cli) = parse(&args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match execute(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            log!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn execute(cli: Cli) -> Result<(), String> {
    let data = PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?)
        .join("t3-dock");
    let t3_home = match std::env::var_os("T3CODE_HOME") {
        Some(home) => PathBuf::from(home),
        None => PathBuf::from(std::env::var_os("USERPROFILE").ok_or("USERPROFILE is not set")?)
            .join(".t3"),
    };
    log::init(&data).map_err(|_| "cannot open the log")?;
    // Panic messages can quote the string being processed, so only the location is logged.
    std::panic::set_hook(Box::new(|info| match info.location() {
        Some(at) => log!("panic at {}:{}", at.file(), at.line()),
        None => log!("panic"),
    }));

    match cli {
        Cli::Auth => t3::auth::setup(&t3_home, &data),
        Cli::Stop => stop(),
        Cli::Run => {
            let instance = win::Instance::claim().ok_or("t3-dock is already running")?;
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| "cannot start the async runtime")?;
            runtime.block_on(run(&instance, t3_home, data))
        }
    }
}

fn stop() -> Result<(), String> {
    if !win::signal_stop() {
        log!("stop: t3-dock is not running");
        return Ok(());
    }
    // Wait for the exit, so the exe can be rebuilt right after.
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(100));
        if !win::signal_stop() {
            log!("stop: stopped");
            return Ok(());
        }
    }
    Err("t3-dock did not stop within 10 seconds".into())
}

async fn run(instance: &win::Instance, t3_home: PathBuf, data: PathBuf) -> Result<(), String> {
    log!("app: started");
    focus::describe();

    let mut app = App::default();
    let (screen_tx, screen_rx) = watch::channel(app.screen());
    let (message_tx, mut messages) = mpsc::channel(64);
    let (view_tx, mut views) = watch::channel(t3::View::default());
    let device = device::start(screen_rx, message_tx)?;
    let client = tokio::spawn(t3::run(t3_home, data, view_tx));

    // One worker, so presses cannot race each other. The newest press wins.
    let (focus_tx, focus_rx) = std::sync::mpsc::channel::<app::Target>();
    std::thread::spawn(move || {
        while let Ok(mut target) = focus_rx.recv() {
            while let Ok(newer) = focus_rx.try_recv() {
                target = newer;
            }
            focus::open(&target);
        }
    });

    let mut stop_poll = interval(Duration::from_millis(250));
    let mut clock = interval(Duration::from_secs(30));
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());
    let mut key_statuses = app.statuses();
    let mut last_tick = SystemTime::now();

    let result = loop {
        tokio::select! {
            _ = &mut ctrl_c => break Ok(()),
            _ = stop_poll.tick() => {
                if instance.stop_requested() {
                    break Ok(());
                }
            }
            _ = clock.tick() => {
                // The wall clock jumping means the PC slept, and the N3 may have lost its images.
                let now = SystemTime::now();
                if now.duration_since(last_tick).is_ok_and(|gap| gap > RESUME_GAP) {
                    log!("app: resumed from sleep, redrawing");
                    app.redraw();
                }
                last_tick = now;
                app.refresh(Timestamp::now());
            }
            changed = views.changed() => {
                if changed.is_err() {
                    break Err("the T3 client stopped".to_owned());
                }
                app.update(views.borrow_and_update().clone(), Timestamp::now());
            }
            message = messages.recv() => match message {
                None => break Err("the device task stopped".to_owned()),
                Some(device::Message::Disconnected) => app.release_all(),
                Some(device::Message::Input(event)) => {
                    log!("input: {event}");
                    if let Some(target) = app.input(event.action)
                        && focus_tx.send(target).is_err()
                    {
                        log!("focus: worker stopped");
                    }
                }
            }
        }
        let statuses = app.statuses();
        if statuses != key_statuses {
            key_statuses = statuses;
            let shown: Vec<String> = statuses
                .iter()
                .map(|status| status.map_or("-".to_owned(), |status| format!("{status:?}")))
                .collect();
            log!("keys: {}", shown.join(" "));
        }
        let screen = app.screen();
        screen_tx.send_if_modified(|shown| {
            let modified = *shown != screen;
            if modified {
                *shown = screen;
            }
            modified
        });
    };

    client.abort();
    drop(screen_tx);
    if timeout(Duration::from_secs(5), device).await.is_err() {
        log!("app: the device did not close in time");
    }
    log!("app: stopped");
    result
}

#[cfg(test)]
mod tests {
    use super::is_session_variable;

    #[test]
    fn session_variables_do_not_reach_the_deep_link() {
        for name in [
            "ELECTRON_RUN_AS_NODE",
            "T3CODE_THREAD_ID",
            "T3CODE_DESKTOP_HOST_SCOPE",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CLAUDECODE",
        ] {
            assert!(is_session_variable(name), "{name}");
        }
        for name in ["T3CODE_HOME", "CLAUDE_CONFIG_DIR", "CODEX_HOME", "PATH"] {
            assert!(!is_session_variable(name), "{name}");
        }
    }
}
