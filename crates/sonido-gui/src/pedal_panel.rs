//! The PEDAL window: how to play the pedal, and the pedal itself over USB.
//!
//! **Controls guide** opens the illustrated guide (`sonido_rigs::guide`, the
//! one `sonido pedal controls` opens) in the browser; it prints as a cheat
//! card. Natively, the window also shows what the connected pedal is playing,
//! its CPU load and its rig list, with each rig's **Play** (and a bank rig's
//! **Erase**), and **Send this rig**, all through `sonido_patch::client` on a
//! worker thread so the editor never waits on USB. In the browser, Send this
//! rig goes over Web Serial ([`crate::web_serial`]).

use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{Context, FontId, RichText};
use sonido_gui_core::theme::SonidoTheme;
use sonido_patch::Patch;
use sonido_patch::pedal::RigRef;

/// What the panel last read from the pedal.
#[derive(Clone, Debug)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // read natively only
pub struct PedalView {
    /// Firmware name and version.
    pub firmware: String,
    /// Link protocol the firmware speaks.
    pub protocol: u8,
    /// The rig playing now.
    pub active: RigRef,
    /// Whole-pedal bypass.
    pub bypassed: bool,
    /// Last second's mean and peak CPU load, as fractions of the block.
    pub load: (f32, f32),
    /// Each bank slot's rig name (`None` when empty).
    pub bank: Vec<Option<String>>,
    /// The factory rigs' names, as the pedal holds them.
    pub factory: Vec<String>,
}

/// A finished pedal job: the pedal as it now stands (when it could be read)
/// and what to tell the user.
struct Outcome {
    view: Option<PedalView>,
    message: Result<String, String>,
}

/// Something to do on the pedal.
#[cfg(not(target_arch = "wasm32"))]
enum Job {
    Refresh,
    Play(RigRef),
    Erase(u8),
    Send(Box<Patch>),
}

/// The PEDAL window's state.
pub struct PedalPanel {
    /// Whether the window is open.
    pub open: bool,
    view: Option<PedalView>,
    busy: bool,
    message: Option<Result<String, String>>,
    /// Bank slot whose Erase was clicked once (the second click erases).
    #[cfg(not(target_arch = "wasm32"))]
    confirm_erase: Option<u8>,
    tx: Sender<Outcome>,
    rx: Receiver<Outcome>,
}

impl Default for PedalPanel {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            open: false,
            view: None,
            busy: false,
            message: None,
            #[cfg(not(target_arch = "wasm32"))]
            confirm_erase: None,
            tx,
            rx,
        }
    }
}

impl PedalPanel {
    /// Whether a pedal job is running.
    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Open the window, reading the pedal (natively) if it hasn't been yet.
    pub fn show_window(&mut self) {
        self.open = true;
        #[cfg(not(target_arch = "wasm32"))]
        if self.view.is_none() && !self.busy {
            self.start(Job::Refresh, None);
        }
    }

    /// Draw the window; `fits` says whether the editor's rig passes the
    /// pedal's check (Export's rule). Returns whether Send this rig was
    /// clicked: the caller then passes the rig to [`send`](Self::send).
    pub fn ui(&mut self, ctx: &Context, fits: bool) -> bool {
        while let Ok(outcome) = self.rx.try_recv() {
            self.busy = false;
            if outcome.view.is_some() {
                self.view = outcome.view;
            }
            self.message = Some(outcome.message);
        }
        if !self.open {
            return false;
        }
        let theme = SonidoTheme::get(ctx);
        let mut open = true;
        let mut send = false;
        egui::Window::new("Pedal")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Open the controls guide").clicked() {
                        self.open_guide(ctx);
                    }
                    ui.label(
                        RichText::new("every knob, toggle and footswitch, illustrated")
                            .font(FontId::monospace(9.0))
                            .weak(),
                    );
                });
                ui.separator();

                #[cfg(not(target_arch = "wasm32"))]
                self.pedal_ui(ui, &theme);

                ui.horizontal(|ui| {
                    let hint = if fits {
                        "Replaces the pedal's rig with this name, or takes its first empty \
                         bank slot, and plays it"
                    } else {
                        "This rig doesn't fit the pedal: see Export > Pedal for why"
                    };
                    if ui
                        .add_enabled(fits && !self.busy, egui::Button::new("Send this rig"))
                        .on_hover_text(hint)
                        .on_disabled_hover_text(hint)
                        .clicked()
                    {
                        send = true;
                    }
                    if self.busy {
                        ui.spinner();
                    }
                });
                if let Some(message) = &self.message {
                    let (text, color) = match message {
                        Ok(m) => (m, theme.colors.green),
                        Err(m) => (m, theme.colors.red),
                    };
                    ui.label(
                        RichText::new(text)
                            .font(FontId::monospace(10.0))
                            .color(color),
                    );
                }
            });
        self.open = open;
        send
    }

    /// The pedal's state and rig list, with Play and Erase.
    #[cfg(not(target_arch = "wasm32"))]
    fn pedal_ui(&mut self, ui: &mut egui::Ui, theme: &SonidoTheme) {
        let ctx = ui.ctx().clone();
        let mut job = None;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("PEDAL")
                    .font(FontId::monospace(10.0))
                    .strong(),
            );
            if ui
                .add_enabled(!self.busy, egui::Button::new("Refresh").small())
                .clicked()
            {
                job = Some(Job::Refresh);
            }
        });
        let Some(view) = &self.view else {
            ui.label(
                RichText::new("Plug the pedal's USB in, then Refresh.")
                    .font(FontId::monospace(10.0))
                    .weak(),
            );
            ui.separator();
            if let Some(job) = job {
                self.start(job, Some(&ctx));
            }
            return;
        };
        ui.label(
            RichText::new(format!(
                "{} · link protocol {} · CPU {:.0}% (peak {:.0}%){}",
                view.firmware,
                view.protocol,
                100.0 * view.load.0,
                100.0 * view.load.1,
                if view.bypassed { " · bypassed" } else { "" }
            ))
            .font(FontId::monospace(10.0))
            .color(theme.colors.text_secondary),
        );
        let row = |ui: &mut egui::Ui, rig: RigRef, label: String, empty: bool| {
            let playing = view.active == rig;
            let text = RichText::new(if playing {
                format!("{label}  (playing)")
            } else {
                label
            })
            .font(FontId::monospace(11.0));
            ui.label(if playing {
                text.color(theme.colors.green)
            } else if empty {
                text.weak()
            } else {
                text
            });
            playing
        };
        egui::ScrollArea::vertical()
            .max_height(280.0)
            .show(ui, |ui| {
                egui::Grid::new("pedal_rigs")
                    .num_columns(2)
                    .spacing([12.0, 2.0])
                    .show(ui, |ui| {
                        let playing = row(ui, RigRef::Jam, "Jam".to_owned(), false);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    !playing && !self.busy,
                                    egui::Button::new("Play").small(),
                                )
                                .clicked()
                            {
                                job = Some(Job::Play(RigRef::Jam));
                            }
                        });
                        ui.end_row();
                        for (slot, name) in (0u8..).zip(&view.bank) {
                            let rig = RigRef::Bank(slot);
                            let label = format!(
                                "Bank {:<2} {}",
                                slot + 1,
                                name.as_deref().unwrap_or("(empty)")
                            );
                            let playing = row(ui, rig, label, name.is_none());
                            ui.horizontal(|ui| {
                                if name.is_some() {
                                    if ui
                                        .add_enabled(
                                            !playing && !self.busy,
                                            egui::Button::new("Play").small(),
                                        )
                                        .clicked()
                                    {
                                        job = Some(Job::Play(rig));
                                    }
                                    let confirming = self.confirm_erase == Some(slot);
                                    let erase = if confirming { "Erase it?" } else { "Erase" };
                                    if ui
                                        .add_enabled(!self.busy, egui::Button::new(erase).small())
                                        .on_hover_text("Empties this bank slot on the pedal")
                                        .clicked()
                                    {
                                        if confirming {
                                            job = Some(Job::Erase(slot));
                                            self.confirm_erase = None;
                                        } else {
                                            self.confirm_erase = Some(slot);
                                        }
                                    }
                                }
                            });
                            ui.end_row();
                        }
                        for (i, name) in (0u8..).zip(&view.factory) {
                            let rig = RigRef::Factory(i);
                            let playing = row(ui, rig, format!("Factory {} {name}", i + 1), false);
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(
                                        !playing && !self.busy,
                                        egui::Button::new("Play").small(),
                                    )
                                    .clicked()
                                {
                                    job = Some(Job::Play(rig));
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
        ui.separator();
        if let Some(job) = job {
            self.start(job, Some(&ctx));
        }
    }

    /// Open the controls guide: natively in the browser from a temp file; on
    /// the web, as a download.
    fn open_guide(&mut self, ctx: &Context) {
        let page = sonido_rigs::guide::html_guide();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let path = std::env::temp_dir().join("sonido-pedal-guide.html");
            match std::fs::write(&path, page) {
                Ok(()) => {
                    ctx.open_url(egui::OpenUrl::new_tab(format!("file://{}", path.display())))
                }
                Err(e) => self.message = Some(Err(format!("Couldn't write the guide: {e}"))),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = ctx;
            let task = rfd::AsyncFileDialog::new()
                .set_file_name("sonido-pedal-guide.html")
                .add_filter("Web page", &["html"])
                .save_file();
            wasm_bindgen_futures::spawn_local(async move {
                if let Some(handle) = task.await {
                    let _ = handle.write(page.as_bytes()).await;
                }
            });
        }
    }

    /// Send `rig` to the pedal.
    pub fn send(&mut self, rig: Patch, ctx: &Context) {
        #[cfg(not(target_arch = "wasm32"))]
        self.start(Job::Send(Box::new(rig)), Some(ctx));
        #[cfg(target_arch = "wasm32")]
        {
            self.busy = true;
            let (tx, ctx) = (self.tx.clone(), ctx.clone());
            wasm_bindgen_futures::spawn_local(async move {
                let message = crate::web_serial::send_rig(&rig)
                    .await
                    .map(|sent| sent_message(&rig, sent.slot, sent.protocol));
                let _ = tx.send(Outcome {
                    view: None,
                    message,
                });
                ctx.request_repaint();
            });
        }
    }

    /// Run `job` on a worker thread; its outcome arrives through `rx`.
    #[cfg(not(target_arch = "wasm32"))]
    fn start(&mut self, job: Job, ctx: Option<&Context>) {
        self.busy = true;
        let (tx, ctx) = (self.tx.clone(), ctx.cloned());
        std::thread::spawn(move || {
            let _ = tx.send(run(job));
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }
}

/// What to say after sending `rig` to bank `slot` of a pedal on `protocol`.
fn sent_message(rig: &Patch, slot: u8, protocol: u8) -> String {
    let mut message = format!("Sent '{}' to bank slot {}: playing", rig.name, slot + 1);
    if let Some(what) = sonido_patch::link::unplayed(rig, protocol) {
        message.push_str(&format!(", without {what}"));
    }
    message
}

/// Do `job` on the pedal, then read it back.
#[cfg(not(target_arch = "wasm32"))]
fn run(job: Job) -> Outcome {
    use sonido_patch::client::open_pedal;
    let done = (|| {
        let mut link = open_pedal(None)?;
        let message = match job {
            Job::Refresh => "Read the pedal".to_owned(),
            Job::Play(rig) => {
                link.activate(rig)?;
                "Playing it".to_owned()
            }
            Job::Erase(slot) => {
                link.erase_slot(slot)?;
                format!("Erased bank slot {}", slot + 1)
            }
            Job::Send(rig) => {
                let slot = link.send_rig(&rig)?;
                sent_message(&rig, slot, link.info()?.protocol)
            }
        };
        let info = link.info()?;
        let status = link.status()?;
        let bank = link.bank()?;
        let factory = sonido_rigs::factory_rigs()
            .into_iter()
            .take(usize::from(info.factory_count))
            .map(|r| r.name)
            .collect();
        Ok::<_, sonido_patch::client::LinkError>((
            PedalView {
                firmware: info.firmware,
                protocol: info.protocol,
                active: status.active,
                bypassed: status.bypassed,
                load: (
                    f32::from(status.load_mean) / 1000.0,
                    f32::from(status.load_peak) / 1000.0,
                ),
                bank,
                factory,
            },
            message,
        ))
    })();
    match done {
        Ok((view, message)) => Outcome {
            view: Some(view),
            message: Ok(message),
        },
        Err(e) => Outcome {
            view: None,
            message: Err(format!("Pedal: {e}")),
        },
    }
}
