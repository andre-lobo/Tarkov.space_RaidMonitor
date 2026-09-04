#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{fs, thread};

use notify::{RecursiveMode, Watcher};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{TrayIconBuilder, TrayIconEvent};
use windows::Win32::System::Diagnostics::Debug::Beep;

const APP_TITLE: &str = "Tarkov.space - Raid Monitor";
const GAME_EXE: &str = "EscapeFromTarkov.exe";

// ---- Colors ----
const BG: egui::Color32 = egui::Color32::from_rgb(0x14, 0x16, 0x1A); // #14161A
const PANEL: egui::Color32 = egui::Color32::from_rgb(0x1D, 0x20, 0x26);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(0xC4, 0xAB, 0x7A); // #C4AB7A
const GREEN: egui::Color32 = egui::Color32::from_rgb(0x3F, 0xD1, 0x7A);
const RED: egui::Color32 = egui::Color32::from_rgb(0xE0, 0x4F, 0x4F);
const TEXT: egui::Color32 = egui::Color32::from_rgb(0xE6, 0xE6, 0xE6);
const MUTED: egui::Color32 = egui::Color32::from_rgb(0x9A, 0x9A, 0x9A);
const SIDE_PMC: egui::Color32 = egui::Color32::from_rgb(0x35, 0x5E, 0x9C); // blue
const SIDE_SCAV: egui::Color32 = egui::Color32::from_rgb(0x9C, 0x54, 0x54); // faded red

fn decode_png(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
}

fn main() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([600.0, 630.0])
        .with_resizable(false)
        .with_maximize_button(false)
        .with_title(APP_TITLE);

    if let Some((rgba, w, h)) = decode_png(include_bytes!("../assets/icon_128.png")) {
        viewport = viewport.with_icon(std::sync::Arc::new(egui::IconData {
            rgba,
            width: w,
            height: h,
        }));
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(APP_TITLE, options, Box::new(|cc| Ok(Box::new(RaidRadar::new(cc)))))
}

#[derive(Clone, PartialEq)]
enum Status {
    Idle,
    Same,
    Different,
    Info,
}

impl Default for Status {
    fn default() -> Self {
        Status::Idle
    }
}

/// One raid-create event parsed from application_000.log.
#[derive(Clone)]
struct Raid {
    ts: String,
    profile_id: String,
    ip: String,
    addr: String, // ip:port
    location: String,
    game_mode: String,
    sid: String,
    mode_tag: String, // PvP / PvE / PvP Season (from "Session mode:")
}

impl Raid {
    fn key(&self) -> String {
        if self.sid.is_empty() {
            format!("{}|{}", self.profile_id, self.addr)
        } else {
            self.sid.clone()
        }
    }
}

/// Local player identity resolved from the logs (cached).
/// PMC profile ids are those "selected" at login (one per game mode: PvE / PvP /
/// PvP Season). Any raid profile id that is NOT a selected profile is a SCAV.
#[derive(Clone, Default)]
struct ProfileCtx {
    pmc_ids: HashSet<String>,
    nick_by_id: HashMap<String, String>,
}

/// Display-ready view of a raid for the UI.
#[derive(Clone, Default)]
struct RaidView {
    time: String,
    ip: String,
    location: String,
    mode: String,
    profile: String,
    mode_tag: String,
    side: String, // "PMC" / "SCAV" / ""
}

fn build_view(r: &Raid, ctx: &ProfileCtx) -> RaidView {
    RaidView {
        time: fmt_time(&r.ts),
        ip: if r.addr.is_empty() { "-".into() } else { r.addr.clone() },
        location: dash(&r.location),
        mode: capitalize(&r.game_mode),
        profile: profile_label(r, ctx),
        mode_tag: r.mode_tag.clone(),
        side: profile_side(r, ctx).to_string(),
    }
}

/// "PMC" / "SCAV" / "" (unresolved).
fn profile_side(r: &Raid, ctx: &ProfileCtx) -> &'static str {
    if r.profile_id.is_empty() || ctx.pmc_ids.is_empty() {
        ""
    } else if ctx.pmc_ids.contains(&r.profile_id) {
        "PMC"
    } else {
        "SCAV"
    }
}

/// Map the raw "Session mode:" value to a friendly tag.
fn mode_label(raw: &str) -> String {
    match raw {
        "PvpSeason" => "PvP Season".into(),
        "Regular" => "PvP".into(),
        "Pve" => "PvE".into(),
        other => other.to_string(),
    }
}

/// "2026-09-04 00:43:23.734" -> "2026-09-04 00:43:23" (24h, drop milliseconds).
fn fmt_time(ts: &str) -> String {
    ts.split('.').next().unwrap_or(ts).trim().to_string()
}

fn capitalize(s: &str) -> String {
    if s.is_empty() {
        return "-".into();
    }
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// PMC -> "nick (ProfileId)"; SCAV -> "SCAV"; unresolved -> shortened id.
fn profile_label(r: &Raid, ctx: &ProfileCtx) -> String {
    if r.profile_id.is_empty() {
        return "-".into();
    }
    if ctx.pmc_ids.contains(&r.profile_id) {
        let nick = ctx
            .nick_by_id
            .get(&r.profile_id)
            .cloned()
            .unwrap_or_else(|| "PMC".into());
        format!("{} ({})", nick, r.profile_id)
    } else if ctx.pmc_ids.is_empty() {
        // Couldn't resolve any PMC id yet -> don't guess SCAV.
        short_id(&r.profile_id)
    } else {
        "SCAV".into()
    }
}

fn dash(s: &str) -> String {
    if s.is_empty() {
        "-".into()
    } else {
        s.to_string()
    }
}

fn short_id(id: &str) -> String {
    if id.is_empty() {
        "-".into()
    } else if id.len() > 12 {
        format!("{}\u{2026}{}", &id[..6], &id[id.len() - 4..])
    } else {
        id.to_string()
    }
}

/// State shared between the UI thread and the background watcher thread.
#[derive(Default)]
struct Shared {
    monitoring: bool,
    game_running: bool,
    in_raid: bool,
    status: Status,
    headline: String,
    note: String,
    session: String,
    current: Option<RaidView>,
    previous: Option<RaidView>,
    history: Vec<RaidView>,
    updates: u64,
}

struct RaidRadar {
    folder: String,
    prev_folder: String,
    shared: Arc<Mutex<Shared>>,
    ctrl: mpsc::Sender<String>,
    sound: Arc<AtomicBool>,
    sound_ui: bool,
    icon: Option<egui::TextureHandle>,
    show_history: bool,
    visible: Arc<AtomicBool>,
}

impl RaidRadar {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let folder = load_config();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let sound = Arc::new(AtomicBool::new(true));
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<String>();

        spawn_worker(
            cc.egui_ctx.clone(),
            shared.clone(),
            sound.clone(),
            folder.clone(),
            ctrl_rx,
        );

        let icon = decode_png(include_bytes!("../assets/icon_128.png")).map(|(rgba, w, h)| {
            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            cc.egui_ctx
                .load_texture("app-icon", img, egui::TextureOptions::LINEAR)
        });

        // System tray icon + menu (runs on its own thread with a Win32 message loop).
        let visible = Arc::new(AtomicBool::new(true));
        spawn_tray(cc.egui_ctx.clone(), visible.clone());

        Self {
            prev_folder: folder.clone(),
            folder,
            shared,
            ctrl: ctrl_tx,
            sound,
            sound_ui: true,
            icon,
            show_history: false,
            visible,
        }
    }
}

/// Creates the tray icon on a dedicated thread that runs a Win32 message loop,
/// so tray/menu events are pumped reliably (independent of the egui loop).
fn spawn_tray(ctx: egui::Context, visible: Arc<AtomicBool>) {
    thread::spawn(move || {
        let (rgba, w, h) = match decode_png(include_bytes!("../assets/icon_32.png")) {
            Some(v) => v,
            None => return,
        };
        let icon = match tray_icon::Icon::from_rgba(rgba, w, h) {
            Ok(i) => i,
            Err(_) => return,
        };
        let menu = Menu::new();
        let toggle = MenuItem::new("Show / Hide", true, None);
        let quit = MenuItem::new("Quit", true, None);
        let _ = menu.append(&toggle);
        let _ = menu.append(&quit);
        let toggle_id = toggle.id().clone();
        let quit_id = quit.id().clone();

        // Keep the tray alive for the life of the thread.
        let _tray = match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(APP_TITLE)
            .with_icon(icon)
            .build()
        {
            Ok(t) => t,
            Err(_) => return,
        };

        // Menu clicks (Show/Hide, Quit).
        {
            let ctx = ctx.clone();
            let visible = visible.clone();
            MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
                if ev.id == quit_id {
                    std::process::exit(0);
                } else if ev.id == toggle_id {
                    let show = !visible.load(Ordering::Relaxed);
                    visible.store(show, Ordering::Relaxed);
                    set_window_visible(show);
                    ctx.request_repaint();
                }
            }));
        }
        // Left-click the tray icon to restore.
        {
            let ctx = ctx.clone();
            let visible = visible.clone();
            TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
                if let TrayIconEvent::Click {
                    button: tray_icon::MouseButton::Left,
                    button_state: tray_icon::MouseButtonState::Up,
                    ..
                } = ev
                {
                    visible.store(true, Ordering::Relaxed);
                    set_window_visible(true);
                    ctx.request_repaint();
                }
            }));
        }

        // Win32 message loop: dispatches tray + menu messages to their handlers.
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, GetMessageW, TranslateMessage, MSG,
            };
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    });
}

impl eframe::App for RaidRadar {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keep ticking so background/tray-driven repaints stay responsive.
        ctx.request_repaint_after(Duration::from_millis(300));

        if self.folder != self.prev_folder {
            self.prev_folder = self.folder.clone();
            save_config(&self.folder);
            let _ = self.ctrl.send(self.folder.clone());
        }
        if self.sound_ui != self.sound.load(Ordering::Relaxed) {
            self.sound.store(self.sound_ui, Ordering::Relaxed);
        }

        let mut style = (*ctx.style()).clone();
        style.visuals.override_text_color = Some(TEXT);
        style.visuals.panel_fill = BG;
        style.visuals.window_fill = BG;
        ctx.set_style(style);

        let snap = {
            let s = self.shared.lock().unwrap();
            (
                s.monitoring,
                s.game_running,
                s.in_raid,
                s.status.clone(),
                s.headline.clone(),
                s.note.clone(),
                s.session.clone(),
                s.current.clone(),
                s.previous.clone(),
                s.history.clone(),
            )
        };
        let (
            monitoring,
            game_running,
            in_raid,
            status,
            headline,
            note,
            session,
            current,
            previous,
            history,
        ) = snap;

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(BG)
                    .inner_margin(egui::Margin::same(20.0)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 10.0);

                // Header
                ui.horizontal(|ui| {
                    if let Some(tex) = &self.icon {
                        ui.add(egui::Image::new(egui::load::SizedTexture::new(
                            tex.id(),
                            egui::vec2(28.0, 28.0),
                        )));
                        ui.add_space(8.0);
                    }
                    ui.label(
                        egui::RichText::new(APP_TITLE)
                            .size(19.0)
                            .strong()
                            .color(ACCENT),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let test = ui.add(
                            egui::Button::new(egui::RichText::new("Test").color(BG).strong())
                                .fill(ACCENT),
                        );
                        if test.clicked() {
                            play_alert();
                        }
                        ui.add_space(6.0);
                        ui.checkbox(&mut self.sound_ui, egui::RichText::new("Sound").color(MUTED));
                    });
                });
                ui.label(
                    egui::RichText::new("Cross-profile same-raid detector (PMC \u{2194} SCAV, by IP)")
                        .size(12.0)
                        .color(MUTED),
                );
                ui.add_space(10.0);

                // Folder input
                ui.label(
                    egui::RichText::new("Tarkov Logs folder")
                        .color(MUTED)
                        .size(13.0),
                );
                ui.horizontal(|ui| {
                    let avail = ui.available_width() - 100.0;
                    ui.add_sized(
                        [avail.max(150.0), 28.0],
                        egui::TextEdit::singleline(&mut self.folder)
                            .hint_text(r"e.g. C:\Program Files\Escape from Tarkov\Logs"),
                    );
                    let browse = ui.add_sized(
                        [90.0, 28.0],
                        egui::Button::new(egui::RichText::new("Browse").color(BG).strong())
                            .fill(ACCENT),
                    );
                    if browse.clicked() {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            self.folder = p.display().to_string();
                        }
                    }
                });

                // Example path hints (Steam vs standalone)
                ui.label(
                    egui::RichText::new(
                        "Steam:  C:\\Steam\\steamapps\\common\\Escape from Tarkov\\build\\Logs",
                    )
                    .size(11.0)
                    .italics()
                    .color(MUTED),
                );
                ui.label(
                    egui::RichText::new("Standalone:  C:\\Battlestate Games\\Escape from Tarkov\\Logs")
                        .size(11.0)
                        .italics()
                        .color(MUTED),
                );

                // Monitoring indicator
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let (txt, col) = if monitoring {
                        ("Monitoring (live)", GREEN)
                    } else {
                        ("Idle \u{2014} set a valid Logs folder", MUTED)
                    };
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(12.0, 14.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 5.0, col);
                    ui.label(egui::RichText::new(txt).color(col).size(13.0));
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(14.0);

                // ---- Result headline (also shows GAME CLOSED) ----
                let game_off = monitoring && !game_running;
                let color = if game_off {
                    RED
                } else {
                    match status {
                        Status::Same => GREEN,
                        Status::Different => RED,
                        Status::Info => ACCENT,
                        Status::Idle => MUTED,
                    }
                };
                let head = if game_off {
                    "GAME CLOSED".to_string()
                } else if headline.is_empty() {
                    "Waiting for a raid\u{2026}".to_string()
                } else {
                    headline
                };
                let sub = if game_off {
                    "Escape from Tarkov is not running.".to_string()
                } else {
                    note
                };

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new(head).size(34.0).strong().color(color));
                            if !sub.is_empty() {
                                ui.add_space(4.0);
                                ui.label(egui::RichText::new(&sub).size(13.0).color(MUTED));
                            }
                        });

                        ui.add_space(14.0);

                        if let Some(cur) = &current {
                            let cur_title = if game_off {
                                "LAST MATCH"
                            } else if in_raid {
                                "CURRENT MATCH"
                            } else {
                                "LAST MATCH"
                            };
                            raid_card(ui, "current", cur_title, cur, color);
                            if let Some(prev) = &previous {
                                ui.add_space(8.0);
                                ui.vertical_centered(|ui| {
                                    ui.label(
                                        egui::RichText::new("compared with previous match")
                                            .size(11.0)
                                            .color(MUTED),
                                    );
                                });
                                ui.add_space(8.0);
                                raid_card(ui, "previous", "PREVIOUS MATCH", prev, TEXT);
                            }
                        }

                        if !session.is_empty() {
                            ui.add_space(12.0);
                            ui.label(
                                egui::RichText::new(format!("Latest session: {}", session))
                                    .size(10.5)
                                    .color(MUTED),
                            );
                        }
                    });
            });

        // ---- Bottom bar: history + minimize to tray ----
        egui::TopBottomPanel::bottom("bottom_bar")
            .frame(
                egui::Frame::none()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(20.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let label = if history.is_empty() {
                        "Match history".to_string()
                    } else {
                        format!("Match history (last {})", history.len())
                    };
                    if ui
                        .add_sized(
                            [180.0, 32.0],
                            egui::Button::new(egui::RichText::new(label).color(BG).strong())
                                .fill(ACCENT),
                        )
                        .clicked()
                    {
                        self.show_history = true;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_sized(
                                [150.0, 32.0],
                                egui::Button::new(
                                    egui::RichText::new("Minimize to tray").color(TEXT),
                                )
                                .fill(PANEL),
                            )
                            .clicked()
                        {
                            self.visible.store(false, Ordering::Relaxed);
                            set_window_visible(false);
                        }
                    });
                });
            });

        // ---- History modal ----
        if self.show_history {
            let screen = ctx.screen_rect();
            let mut close = false;
            egui::Area::new(egui::Id::new("history_dim"))
                .order(egui::Order::Middle)
                .fixed_pos(screen.min)
                .show(ctx, |ui| {
                    ui.painter()
                        .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(160));
                    if ui.allocate_rect(screen, egui::Sense::click()).clicked() {
                        close = true;
                    }
                });
            if close {
                self.show_history = false;
            }

            egui::Window::new(
                egui::RichText::new("Match history (last 10)")
                    .color(ACCENT)
                    .strong(),
            )
            .open(&mut self.show_history)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(BG)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(ctx, |ui| {
                ui.set_width(400.0);
                if history.is_empty() {
                    ui.label(egui::RichText::new("No matches recorded yet.").color(MUTED));
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(420.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (i, r) in history.iter().enumerate() {
                                history_row(ui, i + 1, r);
                                ui.add_space(6.0);
                            }
                        });
                }
            });
        }
    }
}

/// Small rounded tag with a colored background.
fn chip(ui: &mut egui::Ui, text: &str, bg: egui::Color32, fg: egui::Color32) {
    if text.is_empty() {
        return;
    }
    egui::Frame::none()
        .fill(bg)
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).color(fg).size(11.0).strong());
        });
}

/// Renders the side (PMC/SCAV) + game-mode chips right-to-left.
fn tags(ui: &mut egui::Ui, r: &RaidView) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        chip(ui, &r.mode_tag, ACCENT, BG);
        if !r.side.is_empty() {
            ui.add_space(5.0);
            let bg = if r.side == "PMC" { SIDE_PMC } else { SIDE_SCAV };
            chip(ui, &r.side, bg, TEXT);
        }
    });
}

/// Renders a labeled raid card with the IP prominent.
fn raid_card(ui: &mut egui::Ui, id: &str, title: &str, r: &RaidView, ip_color: egui::Color32) {
    egui::Frame::none()
        .fill(PANEL)
        .inner_margin(egui::Margin::same(14.0))
        .rounding(8.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(title).size(11.0).strong().color(ACCENT));
                tags(ui, r);
            });
            ui.add_space(8.0);

            ui.label(
                egui::RichText::new(&r.ip)
                    .size(22.0)
                    .strong()
                    .monospace()
                    .color(ip_color),
            );
            ui.add_space(8.0);

            egui::Grid::new(id)
                .num_columns(2)
                .spacing(egui::vec2(14.0, 7.0))
                .show(ui, |ui| {
                    grid_row(ui, "Location:", &r.location);
                    grid_row(ui, "Mode:", &r.mode);
                    grid_row(ui, "Profile:", &r.profile);
                    grid_row(ui, "Time:", &r.time);
                });
        });
}

fn grid_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(egui::RichText::new(label).size(12.5).color(MUTED));
    ui.label(egui::RichText::new(value).size(13.5).color(TEXT));
    ui.end_row();
}

/// Compact one-entry row used inside the history list.
fn history_row(ui: &mut egui::Ui, idx: usize, r: &RaidView) {
    egui::Frame::none()
        .fill(PANEL)
        .inner_margin(egui::Margin::same(10.0))
        .rounding(6.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{}.", idx))
                        .size(13.0)
                        .strong()
                        .color(MUTED),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(&r.ip)
                        .monospace()
                        .strong()
                        .size(15.0)
                        .color(ACCENT),
                );
                tags(ui, r);
            });
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(format!(
                    "{}  \u{00b7}  {}  \u{00b7}  {}",
                    r.location, r.mode, r.time
                ))
                .size(11.5)
                .color(MUTED),
            );
            ui.label(
                egui::RichText::new(format!("Profile: {}", r.profile))
                    .size(11.0)
                    .color(MUTED),
            );
        });
}

// ---- Background monitoring worker ----

fn spawn_worker(
    ctx: egui::Context,
    shared: Arc<Mutex<Shared>>,
    sound: Arc<AtomicBool>,
    initial_folder: String,
    ctrl_rx: mpsc::Receiver<String>,
) {
    thread::spawn(move || {
        let (ev_tx, ev_rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = ev_tx.send(res);
        })
        .ok();

        let mut folder = initial_folder;
        let mut watched: Option<PathBuf> = None;
        let mut initialized = false;
        let mut last_key = String::new();
        let mut pctx = ProfileCtx::default();
        let mut tick: u32 = 0;

        let apply_folder =
            |new: &str,
             watcher: &mut Option<notify::RecommendedWatcher>,
             watched: &mut Option<PathBuf>| {
                if let (Some(w), Some(old)) = (watcher.as_mut(), watched.as_ref()) {
                    let _ = w.unwatch(old);
                }
                *watched = None;
                let p = Path::new(new.trim());
                if !new.trim().is_empty() && p.is_dir() {
                    if let Some(w) = watcher.as_mut() {
                        if w.watch(p, RecursiveMode::Recursive).is_ok() {
                            *watched = Some(p.to_path_buf());
                        }
                    }
                }
            };

        apply_folder(&folder, &mut watcher, &mut watched);
        run_check(
            &shared,
            &sound,
            &ctx,
            &folder,
            &mut initialized,
            &mut last_key,
            &mut pctx,
        );

        loop {
            let mut folder_changed = false;
            while let Ok(new) = ctrl_rx.try_recv() {
                folder = new;
                folder_changed = true;
            }
            if folder_changed {
                apply_folder(&folder, &mut watcher, &mut watched);
                initialized = false;
                last_key.clear();
                pctx = ProfileCtx::default();
            }

            let mut dirty = folder_changed;
            match ev_rx.recv_timeout(Duration::from_millis(1000)) {
                Ok(_) => {
                    dirty = true;
                    while ev_rx.try_recv().is_ok() {}
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {}
            }

            tick = tick.wrapping_add(1);
            // Re-check on change, and every ~3s (game open/closed state).
            if dirty || tick % 3 == 0 {
                run_check(
                    &shared,
                    &sound,
                    &ctx,
                    &folder,
                    &mut initialized,
                    &mut last_key,
                    &mut pctx,
                );
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn run_check(
    shared: &Arc<Mutex<Shared>>,
    sound: &Arc<AtomicBool>,
    ctx: &egui::Context,
    folder: &str,
    initialized: &mut bool,
    last_key: &mut String,
    pctx: &mut ProfileCtx,
) {
    let ev = evaluate(folder, initialized, last_key, pctx);

    {
        let mut s = shared.lock().unwrap();
        s.monitoring = ev.monitoring;
        s.game_running = ev.game_running;
        s.in_raid = ev.in_raid;
        s.status = ev.status;
        s.headline = ev.headline;
        s.note = ev.note;
        s.session = ev.session;
        s.current = ev.current;
        s.previous = ev.previous;
        s.history = ev.history;
        s.updates = s.updates.wrapping_add(1);
    }
    ctx.request_repaint();

    if ev.is_new_same && sound.load(Ordering::Relaxed) {
        play_alert();
    }
}

struct Eval {
    monitoring: bool,
    game_running: bool,
    in_raid: bool,
    status: Status,
    headline: String,
    note: String,
    session: String,
    current: Option<RaidView>,
    previous: Option<RaidView>,
    history: Vec<RaidView>,
    is_new_same: bool,
}

impl Eval {
    fn simple(monitoring: bool, status: Status, headline: &str, note: &str) -> Self {
        Eval {
            monitoring,
            game_running: is_game_running(),
            in_raid: false,
            status,
            headline: headline.into(),
            note: note.into(),
            session: String::new(),
            current: None,
            previous: None,
            history: Vec::new(),
            is_new_same: false,
        }
    }
}

fn evaluate(
    folder: &str,
    initialized: &mut bool,
    last_key: &mut String,
    pctx: &mut ProfileCtx,
) -> Eval {
    let root = folder.trim();
    if root.is_empty() {
        return Eval::simple(
            false,
            Status::Idle,
            "Waiting\u{2026}",
            "Select your Tarkov Logs folder above to start.",
        );
    }
    let root = Path::new(root);
    if !root.is_dir() {
        return Eval::simple(
            false,
            Status::Info,
            "INVALID FOLDER",
            "The path is not a valid folder.",
        );
    }

    let subs = subfolders_by_mtime_desc(root);
    if subs.is_empty() {
        return Eval::simple(
            true,
            Status::Info,
            "MONITORING",
            "No session folder yet \u{2014} start the game.",
        );
    }
    let session = subs[0]
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    // Resolve local player identity once (until found).
    if pctx.pmc_ids.is_empty() {
        *pctx = resolve_profile(&subs);
    }

    let recent = recent_raids(&subs, 10);

    if recent.is_empty() {
        *initialized = true;
        last_key.clear();
        let note = if find_application_log(&subs[0]).is_none() {
            "No application log yet in this session."
        } else {
            "No raid detected yet."
        };
        let mut e = Eval::simple(true, Status::Info, "MONITORING", note);
        e.session = session;
        return e;
    }

    let history: Vec<RaidView> = recent.iter().map(|r| build_view(r, pctx)).collect();
    let current = &recent[0];
    let cur_view = build_view(current, pctx);
    let new_key = current.key();

    let game_running = is_game_running();
    let in_raid = is_in_raid(&subs[0]);

    // Capture baseline state, then advance it, so the sound only fires on a
    // genuinely new cross-profile "same raid" detection.
    let key_changed = new_key != *last_key;
    let was_initialized = *initialized;
    *last_key = new_key.clone();
    *initialized = true;

    // Not currently in a raid -> you are in the menu.
    if !in_raid {
        return Eval {
            monitoring: true,
            game_running,
            in_raid,
            status: Status::Idle,
            headline: "IN MENU".into(),
            note: "Not in a raid \u{2014} waiting for the next one.".into(),
            session,
            current: Some(cur_view),
            previous: None,
            history,
            is_new_same: false,
        };
    }

    // In a raid. Compare only across profiles (SCAV entering a previous PMC raid).
    let prev = if recent.len() >= 2 { Some(&recent[1]) } else { None };
    let profiles_differ = prev.map_or(false, |p| {
        !current.profile_id.is_empty()
            && !p.profile_id.is_empty()
            && current.profile_id != p.profile_id
    });

    if !profiles_differ {
        return Eval {
            monitoring: true,
            game_running,
            in_raid,
            status: Status::Info,
            headline: "IN RAID".into(),
            note: "In a raid \u{2014} same-raid check runs when you enter as SCAV.".into(),
            session,
            current: Some(cur_view),
            previous: None,
            history,
            is_new_same: false,
        };
    }

    let prev = prev.unwrap();
    let same = current.ip == prev.ip;
    let is_new_same = same && was_initialized && key_changed;

    let (status, headline, note) = if same {
        (
            Status::Same,
            "SAME RAID",
            "You entered the SAME raid as your previous PMC match.",
        )
    } else {
        (
            Status::Different,
            "DIFFERENT RAID",
            "Different raid from your previous PMC match.",
        )
    };

    Eval {
        monitoring: true,
        game_running,
        in_raid,
        status,
        headline: headline.into(),
        note: note.into(),
        session,
        current: Some(cur_view),
        previous: Some(build_view(prev, pctx)),
        history,
        is_new_same,
    }
}

// ---- Profile resolution ----

/// Resolve PMC profile ids (selected at login, one per game mode) and each
/// PMC's nickname (best-effort, from profile/dogtag JSON in the logs).
fn resolve_profile(subs: &[PathBuf]) -> ProfileCtx {
    let mut pmc_ids: HashSet<String> = HashSet::new();

    // Every mode's PMC is logged as a "SelectedProfile" at login. Scan all sessions.
    for folder in subs {
        if let Some(applog) = find_application_log(folder) {
            if let Ok(bytes) = fs::read(&applog) {
                let text = String::from_utf8_lossy(&bytes);
                for line in text.lines() {
                    if line.contains("SelectedProfile") {
                        if let Some(id) = value_after(line, "ProfileId:") {
                            pmc_ids.insert(id);
                        }
                    }
                }
            }
        }
    }

    // Best-effort nickname per PMC id, from the newest sessions only (bounded).
    let mut nick_by_id: HashMap<String, String> = HashMap::new();
    if !pmc_ids.is_empty() {
        'scan: for folder in subs.iter().take(12) {
            if let Ok(rd) = fs::read_dir(folder) {
                for e in rd.flatten() {
                    let p = e.path();
                    let name = p
                        .file_name()
                        .map(|s| s.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    if !name.ends_with(".log") {
                        continue;
                    }
                    // Profile JSON only appears in these logs; skip the huge Unity log.
                    if !(name.contains("backend")
                        || name.contains("application")
                        || name.contains("push-notifications"))
                    {
                        continue;
                    }
                    if let Ok(bytes) = fs::read(&p) {
                        let text = String::from_utf8_lossy(&bytes);
                        for id in &pmc_ids {
                            if nick_by_id.contains_key(id) {
                                continue;
                            }
                            let needle = format!("\"ProfileId\":\"{}\",\"Nickname\":\"", id);
                            if let Some(pos) = text.find(&needle) {
                                let rest = &text[pos + needle.len()..];
                                if let Some(end) = rest.find('"') {
                                    nick_by_id.insert(id.clone(), rest[..end].to_string());
                                }
                            }
                        }
                    }
                    if nick_by_id.len() == pmc_ids.len() {
                        break 'scan;
                    }
                }
            }
        }
    }

    ProfileCtx { pmc_ids, nick_by_id }
}

/// Value after `key` up to the next whitespace (for "ProfileId:xxxx AccountId:yyy").
fn value_after(line: &str, key: &str) -> Option<String> {
    let start = line.find(key)? + key.len();
    let rest = &line[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let v = rest[..end].trim();
    if v.is_empty() {
        None
    } else {
        Some(v.to_string())
    }
}

// ---- Log parsing helpers ----

fn subfolders_by_mtime_desc(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    if let Ok(rd) = fs::read_dir(root) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let mtime = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                v.push((mtime, path));
            }
        }
    }
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.into_iter().map(|(_, p)| p).collect()
}

fn recent_raids(folders_newest_first: &[PathBuf], want: usize) -> Vec<Raid> {
    let mut acc: Vec<Raid> = Vec::new();
    for folder in folders_newest_first {
        let log = match find_application_log(folder) {
            Some(l) => l,
            None => continue,
        };
        if let Ok(mut rs) = parse_raids(&log) {
            rs.reverse();
            for r in rs {
                acc.push(r);
                if acc.len() >= want {
                    return acc;
                }
            }
        }
    }
    acc
}

fn find_network_connection_log(folder: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(folder).ok()?.flatten() {
        let path = entry.path();
        if path.is_file() {
            let name = path.file_name()?.to_string_lossy().to_lowercase();
            if name.contains("network-connection") && name.ends_with(".log") {
                candidates.push(path);
            }
        }
    }
    candidates.sort();
    candidates.into_iter().next()
}

/// Are we currently connected to a raid server (in a raid) in this session?
fn is_in_raid(folder: &Path) -> bool {
    let log = match find_network_connection_log(folder) {
        Some(l) => l,
        None => return false,
    };
    let bytes = fs::read(&log).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    let mut connected = false;
    for line in text.lines() {
        let msg = line.rsplit('|').next().unwrap_or("").trim();
        if msg.starts_with("Connect (address:")
            || msg.starts_with("Enter to the 'Connected' state")
        {
            connected = true;
        } else if msg.starts_with("Disconnect (address:")
            || msg.starts_with("Enter to the 'Disconnected' state")
            || msg.starts_with("Thread was being aborted")
        {
            connected = false;
        }
    }
    connected
}

fn find_application_log(folder: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(folder).ok()?.flatten() {
        let path = entry.path();
        if path.is_file() {
            let name = path.file_name()?.to_string_lossy().to_lowercase();
            if name.contains("application") && name.ends_with(".log") {
                candidates.push(path);
            }
        }
    }
    candidates.sort();
    candidates.into_iter().next()
}

fn field(line: &str, key: &str) -> String {
    let pat = format!("{}: ", key);
    if let Some(start) = line.find(&pat) {
        let rest = &line[start + pat.len()..];
        let end = rest
            .find(|c: char| c == ',' || c == '\'')
            .unwrap_or(rest.len());
        return rest[..end].trim().to_string();
    }
    String::new()
}

fn parse_raids(log: &Path) -> std::io::Result<Vec<Raid>> {
    let bytes = fs::read(log)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut out: Vec<Raid> = Vec::new();
    let mut cur_mode = String::new();
    for line in text.lines() {
        // Track the current session mode (PvP / PvE / PvP Season).
        if let Some(pos) = line.find("Session mode: ") {
            let raw = line[pos + "Session mode: ".len()..].trim();
            cur_mode = mode_label(raw);
        }
        if !line.contains("TRACE-NetworkGameCreate") {
            continue;
        }
        let ip = field(line, "Ip");
        if ip.is_empty() {
            continue;
        }
        let port = field(line, "Port");
        let addr = if port.is_empty() {
            ip.clone()
        } else {
            format!("{}:{}", ip, port)
        };
        let raid = Raid {
            ts: line.split('|').next().unwrap_or("").trim().to_string(),
            profile_id: field(line, "Profileid"),
            ip,
            addr,
            location: field(line, "Location"),
            game_mode: field(line, "GameMode"),
            sid: field(line, "Sid"),
            mode_tag: cur_mode.clone(),
        };
        if let Some(last) = out.last() {
            if last.key() == raid.key() {
                continue;
            }
        }
        out.push(raid);
    }
    Ok(out)
}

// ---- Game process detection ----

fn is_game_running() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    unsafe {
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(_) => return false,
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = false;
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                if name.eq_ignore_ascii_case(GAME_EXE) {
                    found = true;
                    break;
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        found
    }
}

// ---- Native window show/hide (reliable, via Win32) ----

/// Find this process's own top-level window (the one with a title).
fn main_hwnd() -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowThreadProcessId,
    };
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let data = &mut *(lparam.0 as *mut (u32, Option<HWND>));
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == data.0 && GetWindowTextLengthW(hwnd) > 0 {
            data.1 = Some(hwnd);
            return BOOL(0); // stop enumeration
        }
        BOOL(1)
    }
    unsafe {
        let mut data: (u32, Option<HWND>) = (GetCurrentProcessId(), None);
        let _ = EnumWindows(Some(cb), LPARAM(&mut data as *mut _ as isize));
        data.1
    }
}

/// Show or hide the app's own window (reliable, independent of the window title).
fn set_window_visible(show: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetForegroundWindow, ShowWindow, SW_HIDE, SW_SHOW,
    };
    if let Some(hwnd) = main_hwnd() {
        unsafe {
            let _ = ShowWindow(hwnd, if show { SW_SHOW } else { SW_HIDE });
            if show {
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }
}

// ---- Alert sound (native Win32 Beep) ----

fn play_alert() {
    thread::spawn(|| unsafe {
        for _ in 0..2 {
            let _ = Beep(1200, 180);
            let _ = Beep(1650, 240);
        }
    });
}

// ---- Config persistence (last used folder) ----

fn config_path() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    let dir = Path::new(&base).join("Raid IP Monitor");
    let _ = fs::create_dir_all(&dir);
    dir.join("config.txt")
}

fn load_config() -> String {
    let new = config_path();
    if let Ok(s) = fs::read_to_string(&new) {
        return s.trim().to_string();
    }
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    for old in ["Raid IP Monitor", "RaidRadar"] {
        let p = Path::new(&base).join(old).join("config.txt");
        if let Ok(s) = fs::read_to_string(&p) {
            let s = s.trim().to_string();
            let _ = fs::write(&new, &s);
            return s;
        }
    }
    String::new()
}

fn save_config(folder: &str) {
    let _ = fs::write(config_path(), folder.trim());
}
