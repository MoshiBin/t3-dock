//! Owns the N3: draws the screen, reports inputs, reconnects.
//!
//! All HID I/O stays on one thread that lives as long as the app. Windows cancels an overlapped
//! read when the thread that issued it exits, and a multi-thread tokio runtime retires threads
//! after `block_in_place`. So don't use mirajazz's `set_button_image` here, which calls it.
//!
//! Serial numbers and device paths are never logged.

use std::pin::pin;
use std::time::Duration;

use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, RgbImage, imageops};
use mirajazz::device::{Device, DeviceQuery, list_devices};
use mirajazz::error::MirajazzError;
use mirajazz::state::DeviceStateReader;
use mirajazz::types::{DeviceInput, HidDevice};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior, interval, sleep, timeout};

use crate::app::Screen;
use crate::input::{self, SCREEN_KEYS};
use crate::log::log;
use crate::render::{Renderer, SIZE, Tile};

const QUERY: DeviceQuery = DeviceQuery::new(65440, 1, 0x6603, 0x1003);
const JPEG_QUALITY: u8 = 90;
const REPORT_LENGTH: usize = 512;
const KEEP_ALIVE: Duration = Duration::from_secs(15);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY: Duration = Duration::from_secs(3);

pub enum Message {
    Input(input::Event),
    Connected,
    Disconnected,
}

/// Starts the device thread. It blanks the keys and finishes once the screen sender is dropped.
pub fn start(
    screen: watch::Receiver<Screen>,
    messages: mpsc::Sender<Message>,
) -> Result<oneshot::Receiver<()>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "cannot start the device runtime")?;
    let (finished, done) = oneshot::channel();
    std::thread::Builder::new()
        .name("n3".into())
        .spawn(move || {
            runtime.block_on(run(screen, messages));
            let _ = finished.send(());
        })
        .map_err(|_| "cannot start the device thread")?;
    Ok(done)
}

async fn run(screen: watch::Receiver<Screen>, messages: mpsc::Sender<Message>) {
    let mut last_problem = String::new();
    while screen.has_changed().is_ok() {
        let problem = match list_devices(&[QUERY]).await {
            Ok(found) => match found.into_iter().next() {
                Some(info) => {
                    // A task of its own, so a panic in the HID stack becomes a reconnect.
                    let task = tokio::spawn(session(info, screen.clone(), messages.clone()));
                    let started = Instant::now();
                    let problem = match task.await {
                        Ok(Ok(())) => return,
                        Ok(Err(problem)) => problem,
                        Err(_) => "device task panicked".to_owned(),
                    };
                    // Repeats are only suppressed while a connection keeps failing straight away.
                    let was_connected = started.elapsed() > RETRY;
                    let _ = messages.send(Message::Disconnected).await;
                    if was_connected {
                        // A working connection broke, so there is no reason to wait.
                        log!("device: {problem}, reconnecting");
                        last_problem.clear();
                        continue;
                    }
                    problem
                }
                None => "N3 not found".to_owned(),
            },
            Err(error) => format!("listing HID devices failed: {error}"),
        };
        if problem != last_problem {
            log!("device: {problem}, retrying");
            last_problem = problem;
        }
        sleep(RETRY).await;
    }
}

async fn write(
    what: &str,
    operation: impl Future<Output = Result<(), MirajazzError>>,
) -> Result<(), String> {
    timeout(WRITE_TIMEOUT, operation)
        .await
        .map_err(|_| format!("{what} timed out"))?
        .map_err(|error| format!("{what} failed: {error}"))
}

async fn session(
    info: HidDevice,
    mut screen: watch::Receiver<Screen>,
    messages: mpsc::Sender<Message>,
) -> Result<(), String> {
    let device = Device::connect(&info, 3, 9, 3)
        .await
        .map_err(|error| format!("open failed: {error}"))?;
    let mut renderer = Renderer::new();
    let mut sent: [Option<Tile>; SCREEN_KEYS] = Default::default();

    let first = screen.borrow_and_update().clone();
    // Initializing sets brightness to zero, so it has to be set again every time.
    write("brightness", device.set_brightness(first.brightness)).await?;
    write("clear", device.clear_all_button_images()).await?;
    draw(&device, &mut renderer, &mut sent, &first.tiles).await?;
    let mut generation = first.generation;
    let mut brightness = first.brightness;
    log!("device: connected, {SCREEN_KEYS} key images sent");
    let _ = messages.send(Message::Connected).await;

    // Raw reports, because mirajazz's decoded states lose other keys that are held.
    let reader = device.get_reader(|_, _| Ok(DeviceInput::NoData));
    let mut reading = pin!(read_inputs(&reader, &messages));
    let mut keep_alive = interval(KEEP_ALIVE);
    keep_alive.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            result = &mut reading => return result,
            changed = screen.changed() => {
                if changed.is_err() {
                    write("clear", device.clear_all_button_images()).await?;
                    log!("device: keys cleared");
                    return Ok(());
                }
                let next = screen.borrow_and_update().clone();
                if next.generation != generation || next.brightness != brightness {
                    if next.generation != generation {
                        sent = Default::default();
                    }
                    generation = next.generation;
                    brightness = next.brightness;
                    write("brightness", device.set_brightness(brightness)).await?;
                }
                draw(&device, &mut renderer, &mut sent, &next.tiles).await?;
            }
            _ = keep_alive.tick() => write("keep-alive", device.keep_alive()).await?,
        }
    }
}

async fn read_inputs(
    reader: &DeviceStateReader,
    messages: &mpsc::Sender<Message>,
) -> Result<(), String> {
    loop {
        let report = reader
            .raw_read_data(REPORT_LENGTH)
            .await
            .map_err(|error| format!("read failed: {error}"))?;
        if let Some(event) = input::decode(&report)
            && messages.send(Message::Input(event)).await.is_err()
        {
            // The app is shutting down, and the closed screen channel ends the session.
            std::future::pending::<()>().await;
        }
    }
}

/// Sends only the keys that changed.
async fn draw(
    device: &Device,
    renderer: &mut Renderer,
    sent: &mut [Option<Tile>; SCREEN_KEYS],
    tiles: &[Tile; SCREEN_KEYS],
) -> Result<(), String> {
    let mut changed = false;
    for (key, tile) in tiles.iter().enumerate() {
        if sent[key].as_ref() == Some(tile) {
            continue;
        }
        let jpeg = encode(&renderer.render(tile))?;
        // mirajazz's key index is 0-based, and nothing shows until flush().
        write("image", device.write_image(key as u8, &jpeg)).await?;
        sent[key] = Some(tile.clone());
        changed = true;
    }
    if changed {
        write("flush", device.flush()).await?;
    }
    Ok(())
}

/// The N3 wants JPEG rotated 90 degrees clockwise.
fn encode(upright: &RgbImage) -> Result<Vec<u8>, String> {
    let rotated = imageops::rotate90(upright);
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY)
        .encode(rotated.as_raw(), SIZE, SIZE, ExtendedColorType::Rgb8)
        .map_err(|_| "JPEG encoding failed")?;
    Ok(jpeg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_rotated_clockwise() {
        // White on top, which must arrive on the right.
        let upright = RgbImage::from_fn(SIZE, SIZE, |_, y| {
            image::Rgb([if y < 16 { 255 } else { 0 }; 3])
        });
        let sent = image::load_from_memory(&encode(&upright).unwrap())
            .unwrap()
            .into_rgb8();
        assert_eq!(sent.dimensions(), (SIZE, SIZE));
        assert!(sent.get_pixel(60, 32).0[0] > 200);
        assert!(sent.get_pixel(4, 32).0[0] < 50);
    }

    /// Needs an N3 and no running t3-dock: `cargo test read_survives -- --ignored`.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "writes to a connected N3"]
    async fn read_survives_redraws() {
        let info = list_devices(&[QUERY])
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let (screens, screen) = watch::channel(Screen {
            tiles: std::array::from_fn(|key| Tile::numbered(key + 1)),
            brightness: crate::app::DEFAULT_BRIGHTNESS,
            generation: 0,
        });
        let (messages, _inputs) = mpsc::channel(64);
        let mut task = tokio::spawn(session(info, screen, messages));
        for _ in 0..2 {
            sleep(Duration::from_secs(1)).await;
            screens.send_modify(|screen: &mut Screen| screen.tiles[1].pressed ^= true);
        }
        // Longer than tokio keeps an idle blocking thread.
        if let Ok(ended) = timeout(Duration::from_secs(20), &mut task).await {
            panic!("session ended: {ended:?}");
        }
        drop(screens);
        assert!(task.await.unwrap().is_ok());
    }
}
