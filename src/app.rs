use eframe::egui::{self, Color32, RichText, Slider};
use linuxblaster_control::{Equalizer, Output, Preset, State, open};
use std::{
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};

enum Command {
    Refresh,
    Output(Output),
    Feature(u8, bool),
    Eq(Output, Equalizer),
}
enum Event {
    State(State, bool),
    Error(String),
}

pub struct X4App {
    tx: Sender<Command>,
    rx: Receiver<Event>,
    state: Option<State>,
    draft: Option<Equalizer>,
    dirty: bool,
    busy: bool,
    error: Option<String>,
}
impl X4App {
    pub fn new(port: Option<String>, ctx: egui::Context) -> Self {
        let (tx, commands) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut device = None;
            loop {
                let command = match commands.recv_timeout(Duration::from_secs(
                    if device.is_some() { 3 } else { 5 },
                )) {
                    Ok(c) => Some(c),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let result = (|| {
                    if device.is_none() {
                        if matches!(
                            command,
                            Some(
                                Command::Output(_)
                                    | Command::Feature(_, _)
                                    | Command::Eq(_, _)
                            )
                        ) {
                            return Err(
                                linuxblaster_control::protocol::invalid(
                                    "Connection changed; refresh before changing settings",
                                ),
                            );
                        }
                        device = Some(open(port.as_deref())?);
                    }
                    let dev = device.as_mut().unwrap();
                    match &command {
                        Some(Command::Output(output)) => {
                            dev.set_output(*output)?
                        }
                        Some(Command::Feature(bit, enabled)) => {
                            dev.set_feature(*bit, *enabled)?
                        }
                        Some(Command::Eq(output, eq)) => {
                            dev.apply_eq(*output, eq)?
                        }
                        _ => (),
                    }
                    dev.state()
                })();
                let event = match result {
                    Ok(state) => Event::State(state, command.is_some()),
                    Err(error) => {
                        // A partial write is never replayed, including after automatic reconnect.
                        device = None;
                        let prefix = if matches!(
                            command,
                            Some(
                                Command::Output(_)
                                    | Command::Feature(_, _)
                                    | Command::Eq(_, _)
                            )
                        ) {
                            "Change failed; some settings may have changed. Read device state before trying again. "
                        } else {
                            ""
                        };
                        Event::Error(format!("{prefix}{error}"))
                    }
                };
                if events.send(event).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        let app = Self {
            tx,
            rx,
            state: None,
            draft: None,
            dirty: false,
            busy: true,
            error: None,
        };
        let _ = app.tx.send(Command::Refresh);
        app
    }
    fn send(&mut self, command: Command) {
        match self.tx.send(command) {
            Ok(()) => {
                self.busy = true;
                self.error = None;
            }
            Err(_) => {
                self.error = Some(
                    "Device worker stopped. Restart the application.".into(),
                )
            }
        }
    }
    fn events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::State(state, completed) => {
                    let output_changed = self
                        .state
                        .as_ref()
                        .is_some_and(|s| s.output != state.output);
                    if !self.dirty || output_changed || completed {
                        self.draft = state.eq.clone();
                        self.dirty = false;
                    }
                    if output_changed {
                        self.error = Some("Output changed; EQ was refreshed for the active output.".into());
                    }
                    self.state = Some(state);
                    if completed {
                        self.busy = false;
                    }
                }
                Event::Error(error) => {
                    self.error = Some(error);
                    self.state = None;
                    self.busy = false;
                    self.draft = None;
                    self.dirty = false;
                }
            }
        }
    }
    fn content(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.heading("Linuxblaster Control");
                ui.label(
                    RichText::new("SOUND BLASTER X4")
                        .color(Color32::from_rgb(102, 199, 238)),
                );
                if self.busy {
                    ui.spinner();
                }
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Refresh"))
                    .clicked()
                {
                    self.send(Command::Refresh);
                }
            });
            ui.add_space(10.0);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(error) = &self.error { ui.colored_label(Color32::from_rgb(255,180,100), error); ui.separator(); }
            let Some(state) = self.state.clone() else {
                ui.heading(if self.busy { "Connecting to your X4…" } else { "Waiting for Sound Blaster X4" });
                ui.label("Connect the device and close any other controller. This app reconnects automatically.");
                ui.label("Serial access requires the included X4 udev rule and a device replug.");
                return;
            };
            ui.label(format!("Firmware {}  ·  USB 041e:3278", state.firmware));
            ui.add_space(12.0);
            ui.add_enabled_ui(!self.busy, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Output");
                    for (output, label) in [(Output::Speaker,"Speakers"),(Output::Headphone,"Headphones")] {
                        if ui.selectable_label(state.output == output, label).clicked() && state.output != output {
                            self.send(Command::Output(output));
                        }
                    }
                });
                let mut direct = state.feature(5);
                if ui.add_enabled(state.writable_feature(5), egui::Checkbox::new(&mut direct, "Direct Mode")).changed() {
                    self.send(Command::Feature(5, direct));
                }
                let mut high_gain = state.feature(6);
                if ui.add_enabled(state.writable_feature(6) && state.output == Output::Headphone,
                    egui::Checkbox::new(&mut high_gain, "High headphone gain")).changed() { self.send(Command::Feature(6, high_gain)); }
                ui.separator();
                ui.heading("Hardware equalizer");
                ui.label("Changes apply when you press Apply EQ. Bands are numbered until their frequencies are verified.");
                let mut apply = None;
                if let Some(eq) = &mut self.draft {
                    self.dirty |= ui.checkbox(&mut eq.enabled, "Enable EQ").changed();
                    self.dirty |= ui.add(Slider::new(&mut eq.preamp_db, -12.0..=12.0).clamping(egui::SliderClamping::Never).step_by(0.1).text("Preamp (dB)")).changed();
                    ui.horizontal(|ui| {
                        for (i, value) in eq.bands_db.iter_mut().enumerate() {
                            ui.vertical(|ui| {
                                ui.label(format!("Band {}", i + 1));
                                self.dirty |= ui.add(Slider::new(value, -12.0..=12.0).clamping(egui::SliderClamping::Never).step_by(0.1).vertical()).changed();
                                ui.label("dB");
                            });
                        }
                    });
                    let validation = eq.validate();
                    if let Err(error) = &validation { ui.colored_label(Color32::YELLOW, error.to_string()); }
                    ui.horizontal(|ui| {
                        if ui.add_enabled(self.dirty && validation.is_ok(), egui::Button::new("Apply EQ")).clicked() { apply = Some(eq.clone()); }
                        if ui.button("Discard edits").clicked() { *eq = state.eq.clone().unwrap(); self.dirty = false; }
                    });
                } else { ui.label("This device does not advertise hardware EQ."); }
                if let Some(eq) = apply { self.send(Command::Eq(state.output, eq)); }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.add_enabled(self.draft.is_some(), egui::Button::new("Save EQ preset…")).clicked()
                        && let Some(path) = rfd::FileDialog::new().add_filter("X4 EQ preset", &["json"]).set_file_name("x4-eq.json").save_file() {
                        let result = (|| -> linuxblaster_control::protocol::Result<()> {
                            let eq = self.draft.clone().unwrap(); eq.validate()?;
                            std::fs::write(path, serde_json::to_vec_pretty(&Preset::new(state.output, eq))?)?;
                            Ok(())
                        })();
                        if let Err(e) = result { self.error = Some(e.to_string()); }
                    }
                    if ui.add_enabled(state.eq_supported, egui::Button::new("Load EQ preset…")).clicked()
                        && let Some(path) = rfd::FileDialog::new().add_filter("X4 EQ preset", &["json"]).pick_file() {
                        let result = (|| -> linuxblaster_control::protocol::Result<Preset> {
                            let preset: Preset = serde_json::from_slice(&std::fs::read(path)?)?;
                            preset.validate(state.output)?;
                            Ok(preset)
                        })();
                        match result {
                            Ok(preset) => { self.draft = Some(preset.eq); self.dirty = true; }
                            Err(e) => self.error = Some(e.to_string()),
                        }
                    }
                });
                ui.label("Presets contain EQ for the selected output. Loading previews the settings before Apply EQ.");
                ui.add_space(8.0);
                ui.small("Use your desktop sound settings for volume, recording and sample rate.");
            });
        });
    }
}
impl eframe::App for X4App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.events();
        self.content(ctx);
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_disconnected_and_connected_without_device_access() {
        let (tx, _) = mpsc::channel();
        let (_, rx) = mpsc::channel();
        let mut app = X4App {
            tx,
            rx,
            state: None,
            draft: None,
            dirty: false,
            busy: false,
            error: None,
        };
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| app.content(ctx));
        let eq = Equalizer {
            enabled: true,
            preamp_db: -9.0,
            bands_db: [0.0; 10],
        };
        app.state = Some(State {
            firmware: "test".into(),
            output: Output::Headphone,
            supported_features: 0x60,
            reset_features: 0,
            features: 0x40,
            eq_supported: true,
            eq: Some(eq.clone()),
        });
        app.draft = Some(eq);
        let _ = ctx.run(egui::RawInput::default(), |ctx| app.content(ctx));
    }
}
