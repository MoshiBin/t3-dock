//! Owns the Stream Dock: draws the screen, reports inputs, reconnects.
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
use mirajazz::device::{Device, list_devices};
use mirajazz::error::MirajazzError;
use mirajazz::state::DeviceStateReader;
use mirajazz::types::{DeviceInput, HidDevice};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior, interval, sleep, timeout};

use crate::app::Screen;
use crate::input;
use crate::log::log;
use crate::model::{self, Model, Rotation};
use crate::render::{Renderer, Tile};

const JPEG_QUALITY: u8 = 90;
const REPORT_LENGTH: usize = 512;
const KEEP_ALIVE: Duration = Duration::from_secs(15);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY: Duration = Duration::from_secs(3);

pub enum Message {
    Input(input::Event),
    Connected(Model),
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
        .name("dock".into())
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
        let problem = match list_devices(&model::QUERIES).await {
            Ok(found) => match first_known(found) {
                Some((info, model)) => {
                    // A task of its own, so a panic in the HID stack becomes a reconnect.
                    let task = tokio::spawn(session(info, model, screen.clone(), messages.clone()));
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
                None => "no Stream Dock found".to_owned(),
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

/// One device, in the order of `model::QUERIES`, so the choice doesn't change between scans.
fn first_known(found: impl IntoIterator<Item = HidDevice>) -> Option<(HidDevice, Model)> {
    found
        .into_iter()
        .filter_map(|info| Some((Model::from_ids(info.vendor_id, info.product_id)?, info)))
        .min_by_key(|(model, _)| *model as u8)
        .map(|(model, info)| (info, model))
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
    model: Model,
    mut screen: watch::Receiver<Screen>,
    messages: mpsc::Sender<Message>,
) -> Result<(), String> {
    let (protocol, keys, encoders) = model.connect_args();
    let device = Device::connect(&info, protocol, keys, encoders)
        .await
        .map_err(|error| format!("open failed: {error}"))?;
    let mut painter = Painter::new(model);

    let first = screen.borrow_and_update().clone();
    // Initializing sets brightness to zero, so it has to be set again every time.
    write("brightness", device.set_brightness(first.brightness)).await?;
    write("clear", device.clear_all_button_images()).await?;
    painter.draw(&device, &first).await?;
    let mut generation = first.generation;
    let mut brightness = first.brightness;
    log!("device: {} connected", model.name());
    // The app answers with a screen for this model, which is drawn then.
    let _ = messages.send(Message::Connected(model)).await;

    // Raw reports, because mirajazz's decoded states lose other keys that are held.
    let reader = device.get_reader(|_, _| Ok(DeviceInput::NoData));
    let mut reading = pin!(read_inputs(&reader, model, &messages));
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
                        painter.forget();
                    }
                    generation = next.generation;
                    brightness = next.brightness;
                    write("brightness", device.set_brightness(brightness)).await?;
                }
                painter.draw(&device, &next).await?;
            }
            _ = keep_alive.tick() => write("keep-alive", device.keep_alive()).await?,
        }
    }
}

async fn read_inputs(
    reader: &DeviceStateReader,
    model: Model,
    messages: &mpsc::Sender<Message>,
) -> Result<(), String> {
    loop {
        let report = reader
            .raw_read_data(REPORT_LENGTH)
            .await
            .map_err(|error| format!("read failed: {error}"))?;
        if let Some(event) = input::decode(&report, model)
            && messages.send(Message::Input(event)).await.is_err()
        {
            // The app is shutting down, and the closed screen channel ends the session.
            std::future::pending::<()>().await;
        }
    }
}

/// Draws one model's tiles and remembers what each image slot shows.
struct Painter {
    model: Model,
    renderer: Renderer,
    /// By image slot.
    sent: Vec<Option<Tile>>,
}

impl Painter {
    fn new(model: Model) -> Self {
        let layout = model.layout();
        Self {
            model,
            renderer: Renderer::new(),
            sent: vec![None; layout.keys + layout.side],
        }
    }

    /// Draws everything again next time.
    fn forget(&mut self) {
        self.sent.fill(None);
    }

    /// Sends only the images that changed. A screen for another model waits for the app.
    async fn draw(&mut self, device: &Device, screen: &Screen) -> Result<(), String> {
        if screen.layout != self.model.layout() {
            return Ok(());
        }
        let keys = screen
            .tiles
            .iter()
            .enumerate()
            .map(|(key, tile)| (self.model.key_slot(key), self.model.key_size(), tile));
        let side = screen
            .side
            .iter()
            .enumerate()
            .map(|(tile, image)| (self.model.side_slot(tile), self.model.side_size(), image));
        let mut changed = false;
        for (slot, size, tile) in keys.chain(side) {
            let sent = &mut self.sent[usize::from(slot)];
            if sent.as_ref() == Some(tile) {
                continue;
            }
            let jpeg = encode(&self.renderer.render(tile, size), self.model.rotation())?;
            // mirajazz's slot index is 0-based, and nothing shows until flush().
            write("image", device.write_image(slot, &jpeg)).await?;
            *sent = Some(tile.clone());
            changed = true;
        }
        if changed {
            write("flush", device.flush()).await?;
        }
        Ok(())
    }
}

/// The N3 wants JPEG rotated 90 degrees clockwise, the 293S counterclockwise.
fn encode(upright: &RgbImage, rotation: Rotation) -> Result<Vec<u8>, String> {
    let rotated = match rotation {
        Rotation::Clockwise => imageops::rotate90(upright),
        Rotation::Counterclockwise => imageops::rotate270(upright),
    };
    let mut jpeg = Vec::new();
    let (width, height) = rotated.dimensions();
    JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY)
        .encode(rotated.as_raw(), width, height, ExtendedColorType::Rgb8)
        .map_err(|_| "JPEG encoding failed")?;
    Ok(jpeg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_turn_each_models_way() {
        // White on top, which must arrive on the right for the N3 and on the left for the 293S.
        for (size, rotation, white, black) in [
            (64, Rotation::Clockwise, 60, 4),
            (96, Rotation::Counterclockwise, 4, 92),
        ] {
            let upright = RgbImage::from_fn(size, size, |_, y| {
                image::Rgb([if y < size / 4 { 255 } else { 0 }; 3])
            });
            let sent = image::load_from_memory(&encode(&upright, rotation).unwrap())
                .unwrap()
                .into_rgb8();
            assert_eq!(sent.dimensions(), (size, size));
            assert!(sent.get_pixel(white, size / 2).0[0] > 200);
            assert!(sent.get_pixel(black, size / 2).0[0] < 50);
        }
    }

    async fn connected() -> (HidDevice, Model) {
        let found = list_devices(&model::QUERIES).await.unwrap();
        first_known(found).expect("no Stream Dock connected")
    }

    /// Numbered tiles in reading order, so every key and side tile has its number.
    fn numbered(model: Model) -> Screen {
        let layout = model.layout();
        Screen {
            layout,
            tiles: (0..layout.keys)
                .map(|key| Tile::numbered(key + 1))
                .collect(),
            side: (0..layout.side)
                .map(|tile| Tile::numbered(layout.keys + tile + 1))
                .collect(),
            brightness: crate::config::Preferences::default().default_brightness,
            generation: 0,
        }
    }

    /// Needs a Stream Dock and no running t3-dock: `cargo test read_survives -- --ignored`.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "writes to a connected Stream Dock"]
    async fn read_survives_redraws() {
        let (info, model) = connected().await;
        let (screens, screen) = watch::channel(numbered(model));
        let (messages, _inputs) = mpsc::channel(64);
        let mut task = tokio::spawn(session(info, model, screen, messages));
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

    /// Shows numbered tiles and prints every press for a minute (or `T3_DOCK_SHOW_SECONDS`), to
    /// check the key order: `cargo test show_numbers -- --ignored --nocapture`.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "writes to a connected Stream Dock"]
    async fn show_numbers() {
        let (info, model) = connected().await;
        let (screens, screen) = watch::channel(numbered(model));
        let (messages, mut inputs) = mpsc::channel(64);
        let task = tokio::spawn(session(info, model, screen, messages));
        let seconds = std::env::var("T3_DOCK_SHOW_SECONDS")
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(60);
        let until = Instant::now() + Duration::from_secs(seconds);
        while let Ok(Some(message)) = tokio::time::timeout_at(until, inputs.recv()).await {
            if let Message::Input(event) = message {
                println!("{event}");
            }
        }
        drop(screens);
        assert!(task.await.unwrap().is_ok());
    }

    /// A frame that shows which edges of an image a slot shows: red top, green right, blue
    /// bottom, yellow left, and a white ring every 6 pixels inward.
    fn frame(size: u32) -> RgbImage {
        RgbImage::from_fn(size, size, |x, y| {
            let inset = x.min(y).min(size - 1 - x).min(size - 1 - y);
            let color = if inset < 2 {
                match (y < 2, x >= size - 2, y >= size - 2) {
                    (true, _, _) => [255, 0, 0],
                    (_, true, _) => [0, 255, 0],
                    (_, _, true) => [0, 80, 255],
                    _ => [255, 220, 0],
                }
            } else if inset % 6 == 0 {
                [255, 255, 255]
            } else {
                [20, 20, 20]
            };
            image::Rgb(color)
        })
    }

    /// Frames at several sizes, to find how many pixels each slot shows:
    /// `cargo test show_frames -- --ignored --nocapture`.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "writes to a connected Stream Dock"]
    async fn show_frames() {
        let (info, model) = connected().await;
        let (protocol, keys, encoders) = model.connect_args();
        let device = Device::connect(&info, protocol, keys, encoders)
            .await
            .unwrap();
        device.set_brightness(60).await.unwrap();
        device.clear_all_button_images().await.unwrap();
        let layout = model.layout();
        // Keys at their size, side tiles at 96, 88 and 82 from the top.
        let mut slots: Vec<(u8, u32)> = (0..layout.keys)
            .map(|key| (model.key_slot(key), model.key_size()))
            .collect();
        slots.extend(
            [96, 88, 82]
                .into_iter()
                .enumerate()
                .take(layout.side)
                .map(|(tile, size)| (model.side_slot(tile), size)),
        );
        for (slot, size) in slots {
            let jpeg = encode(&frame(size), model.rotation()).unwrap();
            device.write_image(slot, &jpeg).await.unwrap();
        }
        device.flush().await.unwrap();
        let seconds = std::env::var("T3_DOCK_SHOW_SECONDS")
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(60);
        let mut keep_alive = interval(KEEP_ALIVE);
        let until = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < until {
            keep_alive.tick().await;
            device.keep_alive().await.unwrap();
        }
        device.clear_all_button_images().await.unwrap();
    }
}
