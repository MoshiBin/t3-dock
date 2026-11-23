//! The settings window: rebinds the keys and dials and sets the preferences.
//!
//! It runs as its own process (`t3-dock settings`), started from the tray menu, so the dock keeps
//! its own thread layout and a closed window leaves nothing behind. It only writes `config.json`,
//! which the running dock reloads, and the start-at-login value in the registry.

use std::path::{Path, PathBuf};

use eframe::egui::{
    self, Align, Button, CentralPanel, Checkbox, ComboBox, Grid, Layout, Modal, Panel, RichText,
    ScrollArea, Slider, ViewportBuilder, ViewportCommand,
};

use crate::config::{self, Bindings, Config, DialBinding, Press, Turn};
use crate::log::log;
use crate::{login, tray, win};

const SETTINGS_EVENT: &str = r"Local\t3-dock-settings";
const TITLE: &str = "t3-dock settings";
const COMBO_WIDTH: f32 = 240.0;
/// Tall enough for every action, so the list never scrolls.
const COMBO_HEIGHT: f32 = 420.0;

pub fn run(data: &Path) -> Result<(), String> {
    // One window. A second start brings the first one forward.
    let Some(_instance) = win::Instance::claim_named(SETTINGS_EVENT) else {
        let window = std::env::current_exe()
            .ok()
            .and_then(|exe| win::main_window(exe.to_str()?));
        let found = window.is_some();
        let raised = window.is_some_and(|window| window.raise());
        log!("settings: already open, found={found} raised={raised}");
        return Ok(());
    };

    let path = data.join(config::FILE);
    let window = Settings::new(path.clone(), Config::load(&path));
    let icon = egui::IconData {
        rgba: tray::icon_pixels(),
        width: tray::ICON_SIZE,
        height: tray::ICON_SIZE,
    };
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size([600.0, 720.0])
            .with_min_inner_size([480.0, 360.0])
            .with_icon(icon),
        ..Default::default()
    };
    log!("settings: opened");
    eframe::run_native(TITLE, options, Box::new(|_| Ok(Box::new(window))))
        .map_err(|_| "cannot open the settings window".to_owned())?;
    log!("settings: closed");
    Ok(())
}

struct Settings {
    path: PathBuf,
    saved: Config,
    draft: Config,
    login_saved: bool,
    login_draft: bool,
    /// The result of the last save, and whether it failed.
    status: Option<(String, bool)>,
    asking_to_close: bool,
    closing: bool,
}

impl Settings {
    fn new(path: PathBuf, config: Config) -> Self {
        let login = login::is_on();
        Self {
            path,
            saved: config,
            draft: config,
            login_saved: login,
            login_draft: login,
            status: None,
            asking_to_close: false,
            closing: false,
        }
    }

    fn dirty(&self) -> bool {
        self.draft != self.saved || self.login_draft != self.login_saved
    }

    fn save(&mut self) -> bool {
        if let Err(error) = self.draft.save(&self.path) {
            log!("settings: {error}");
            self.status = Some((format!("Not saved: {error}."), true));
            return false;
        }
        self.saved = self.draft;
        if self.login_draft != self.login_saved {
            let done = login::set(self.login_draft);
            log!("settings: start at login on={} ok={done}", self.login_draft);
            self.login_saved = login::is_on();
            self.login_draft = self.login_saved;
            if !done {
                self.status = Some(("Saved, but start at login could not change.".into(), true));
                return true;
            }
        }
        log!("settings: saved");
        let text = if win::is_running() {
            "Saved. The dock uses the new settings now."
        } else {
            "Saved. The dock uses them when it starts."
        };
        self.status = Some((text.into(), false));
        true
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.heading("Controls");
        ui.label("The six screen keys always open their thread.");
        ui.add_space(6.0);

        let defaults = Bindings::default();
        let bindings = &mut self.draft.bindings;
        Grid::new("controls")
            .num_columns(4)
            .spacing([12.0, 8.0])
            .striped(true)
            .show(ui, |ui| {
                section(ui, "Keys under the screens");
                press_row(
                    ui,
                    "Left key",
                    "Left key",
                    &mut bindings.left_key,
                    defaults.left_key,
                );
                press_row(
                    ui,
                    "Middle key",
                    "Middle key",
                    &mut bindings.middle_key,
                    defaults.middle_key,
                );
                press_row(
                    ui,
                    "Right key",
                    "Right key",
                    &mut bindings.right_key,
                    defaults.right_key,
                );
                dial_rows(ui, "Top dial", &mut bindings.top_dial, defaults.top_dial);
                dial_rows(
                    ui,
                    "Bottom-left dial",
                    &mut bindings.left_dial,
                    defaults.left_dial,
                );
                dial_rows(
                    ui,
                    "Bottom-right dial",
                    &mut bindings.right_dial,
                    defaults.right_dial,
                );
            });
    }

    fn preferences(&mut self, ui: &mut egui::Ui) {
        ui.heading("Preferences");
        ui.add_space(6.0);
        let prefs = &mut self.draft.preferences;
        Grid::new("preferences")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label("Reset brightness to");
                let (low, high) = config::BRIGHTNESS_RANGE;
                ui.add(Slider::new(&mut prefs.default_brightness, low..=high).suffix("%"));
                ui.end_row();

                ui.label("Brightness per dial click");
                let (low, high) = config::STEP_RANGE;
                ui.add(Slider::new(&mut prefs.brightness_step, low..=high).suffix("%"));
                ui.end_row();

                ui.label("Show messages for");
                let (low, high) = config::MESSAGE_RANGE;
                let mut seconds = prefs.message_ms as f32 / 1000.0;
                let slider = Slider::new(&mut seconds, low as f32 / 1000.0..=high as f32 / 1000.0)
                    .step_by(0.1)
                    .fixed_decimals(1)
                    .suffix(" s");
                if ui.add(slider).changed() {
                    prefs.message_ms = (seconds * 1000.0).round() as u32;
                }
                ui.end_row();
            });
        ui.add_space(4.0);
        ui.add(Checkbox::new(
            &mut prefs.hide_idle,
            "Hide idle threads, so keys show only live, finished and waiting ones",
        ));
        ui.add(Checkbox::new(
            &mut self.login_draft,
            "Start t3-dock when I sign in to Windows",
        ));
    }

    fn actions(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .button("Restore defaults")
                .on_hover_text("Default controls and preferences. Nothing is saved yet.")
                .clicked()
            {
                self.draft = Config::default();
                // Off by default, like the tray item.
                self.login_draft = false;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let dirty = self.dirty();
                if ui.add_enabled(dirty, Button::new("Save")).clicked() {
                    self.save();
                }
                if ui.add_enabled(dirty, Button::new("Revert")).clicked() {
                    self.draft = self.saved;
                    self.login_draft = self.login_saved;
                    self.status = None;
                }
                if dirty {
                    ui.label(RichText::new("Unsaved changes").weak());
                } else if let Some((text, failed)) = &self.status {
                    let text = RichText::new(text.as_str());
                    ui.label(if *failed {
                        text.color(ui.visuals().error_fg_color)
                    } else {
                        text.weak()
                    });
                }
            });
        });
    }

    fn close_prompt(&mut self, ctx: &egui::Context) {
        if !self.asking_to_close {
            return;
        }
        let modal = Modal::new(egui::Id::unique("close")).show(ctx, |ui| {
            ui.set_width(300.0);
            ui.heading("Save your changes?");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() && self.save() {
                    self.close(ui.ctx());
                }
                if ui.button("Don't save").clicked() {
                    self.close(ui.ctx());
                }
                if ui.button("Cancel").clicked() {
                    self.asking_to_close = false;
                }
            });
        });
        if modal.should_close() {
            self.asking_to_close = false;
        }
    }

    fn close(&mut self, ctx: &egui::Context) {
        self.closing = true;
        self.asking_to_close = false;
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
}

impl eframe::App for Settings {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|input| input.viewport().close_requested()) && self.dirty() && !self.closing {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.asking_to_close = true;
        }
        let save_key = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::S);
        if ctx.input_mut(|input| input.consume_shortcut(&save_key)) && self.dirty() {
            self.save();
        }

        Panel::bottom("actions").show(ui, |ui| {
            ui.add_space(6.0);
            self.actions(ui);
            ui.add_space(6.0);
        });
        CentralPanel::default().show(ui, |ui| {
            ScrollArea::vertical().show(ui, |ui| {
                self.controls(ui);
                ui.add_space(14.0);
                ui.separator();
                ui.add_space(6.0);
                self.preferences(ui);
            });
        });
        self.close_prompt(&ctx);
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).strong());
    ui.end_row();
}

/// A button to undo one row, shown only when the row differs from the default.
fn reset_button<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, default: T) {
    if *value != default {
        if ui
            .small_button("Default")
            .on_hover_text("Use the default for this control")
            .clicked()
        {
            *value = default;
        }
    } else {
        ui.label("");
    }
}

fn press_combo(ui: &mut egui::Ui, id: &str, value: &mut Press) {
    ComboBox::from_id_salt(id)
        .width(COMBO_WIDTH)
        .height(COMBO_HEIGHT)
        .selected_text(value.label())
        .show_ui(ui, |ui| {
            for option in Press::ALL {
                ui.selectable_value(value, option, option.label());
            }
        });
}

fn turn_combo(ui: &mut egui::Ui, id: &str, value: &mut Turn) {
    ComboBox::from_id_salt(id)
        .width(COMBO_WIDTH)
        .height(COMBO_HEIGHT)
        .selected_text(value.label())
        .show_ui(ui, |ui| {
            for option in Turn::ALL {
                ui.selectable_value(value, option, option.label());
            }
        });
}

fn press_row(ui: &mut egui::Ui, id: &str, name: &str, value: &mut Press, default: Press) {
    ui.label(format!("    {name}"));
    press_combo(ui, id, value);
    ui.label("");
    reset_button(ui, value, default);
    ui.end_row();
}

fn dial_rows(ui: &mut egui::Ui, name: &str, dial: &mut DialBinding, default: DialBinding) {
    section(ui, name);

    ui.label("    Turn");
    turn_combo(ui, &format!("{name} turn"), &mut dial.turn);
    ui.add_enabled(
        dial.turn != Turn::Nothing,
        Checkbox::new(&mut dial.reverse, "Reverse"),
    )
    .on_hover_text("Swap left and right turns");
    if dial.turn != default.turn || dial.reverse != default.reverse {
        if ui
            .small_button("Default")
            .on_hover_text("Use the default for this dial")
            .clicked()
        {
            dial.turn = default.turn;
            dial.reverse = default.reverse;
        }
    } else {
        ui.label("");
    }
    ui.end_row();

    press_row(
        ui,
        &format!("{name} press"),
        "Press",
        &mut dial.press,
        default.press,
    );
}
