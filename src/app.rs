use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, CornerRadius, FontFamily, FontId, Layout, Rect, RichText, Sense, Stroke,
    TextureHandle, TextureOptions, Vec2, pos2, vec2,
};

use crate::config::{Contact, random_password, thumb_path};
use crate::input::vk_for;
use crate::net::{Net, Presence, ViewStatus, Viewer};
use crate::proto::{Input, WRONG_PASSWORD};
use crate::shortid;
use crate::updater::{self, UpdateState, Updater};

// Brand colours sampled from the logo: #3A1EF2 (deep) -> #6332FB (primary) -> #A368FC (light).
const BG: Color32 = Color32::from_rgb(23, 22, 29);
const SURFACE: Color32 = Color32::from_rgb(33, 31, 42);
const SURFACE2: Color32 = Color32::from_rgb(42, 40, 54);
const BORDER: Color32 = Color32::from_rgb(54, 51, 71);
const TEXT: Color32 = Color32::from_rgb(237, 236, 242);
const MUTED: Color32 = Color32::from_rgb(160, 156, 178);
const ACCENT: Color32 = Color32::from_rgb(99, 50, 251);
const ACCENT_LIGHT: Color32 = Color32::from_rgb(163, 104, 252);
const ID_COLOR: Color32 = Color32::from_rgb(139, 92, 252);
const DANGER: Color32 = Color32::from_rgb(229, 72, 77);
const DANGER_TEXT: Color32 = Color32::from_rgb(255, 150, 150);
const GREEN: Color32 = Color32::from_rgb(48, 164, 108);
const AMBER: Color32 = Color32::from_rgb(245, 165, 36);

const CARD: Vec2 = vec2(236.0, 164.0);
const THUMB_W: usize = 320;

fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

/// A connection the user asked for that still needs (or is waiting on) a password.
struct Pending {
    target: String,
    label: String,
    password: String,
    remember: bool,
    error: Option<String>,
    focus: bool,
}

struct Session {
    viewer: Arc<Mutex<Viewer>>,
    label: String,
    password: String,
    remember: bool,
    texture: Option<TextureHandle>,
    /// CPU copy of the remote screen, used for the contact-card thumbnail.
    fb: Vec<u8>,
    fb_size: [usize; 2],
    scroll: Vec2,
    saved_contact: bool,
    fullscreen: bool,
    stats_at: Instant,
    stats_bytes: u64,
    stats_frames: u64,
    fps: f32,
    kbps: f32,
    /// Modifier keys currently held down on the remote, so we can release them explicitly.
    sent_mods: egui::Modifiers,
}

#[derive(Default)]
struct SettingsDraft {
    name: String,
    password: String,
    ask_on_connect: bool,
    start_with_windows: bool,
}

impl SettingsDraft {
    fn from_cfg(cfg: &crate::config::Config) -> SettingsDraft {
        SettingsDraft {
            name: cfg.device_name.clone(),
            password: cfg.password.clone(),
            ask_on_connect: cfg.ask_on_connect,
            start_with_windows: cfg.start_with_windows,
        }
    }
}

pub struct App {
    net: Net,
    address: String,
    show_pw: bool,
    error: Option<String>,
    pending: Option<Pending>,
    session: Option<Session>,
    settings: Option<SettingsDraft>,
    confirm_delete: Option<Contact>,
    /// Machine being renamed: (id, draft name, focus the field next frame).
    rename: Option<(String, String, bool)>,
    thumbs: HashMap<String, Option<TextureHandle>>,
    gradients: HashMap<String, TextureHandle>,
    logo: TextureHandle,
    updater: Updater,
    /// True once the tray icon is running, so closing the window hides to tray instead of exiting.
    close_to_tray: bool,
    /// Whether the local keyboard is currently being sent to the remote.
    capturing_keys: bool,
    #[cfg(debug_assertions)]
    preview: Option<(String, Instant)>,
    #[cfg(debug_assertions)]
    font_test: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<App, Box<dyn std::error::Error + Send + Sync>> {
        setup_style(&cc.egui_ctx);
        let net = Net::start(crate::config::Config::load(), cc.egui_ctx.clone())?;
        // Keep the Windows Run entry in sync with the saved setting.
        crate::autostart::set_enabled(net.shared.lock().unwrap().cfg.start_with_windows);
        crate::tray::start(cc.egui_ctx.clone());
        crate::singleinstance::start_show_listener(cc.egui_ctx.clone());
        let updater = Updater::start(net.runtime(), cc.egui_ctx.clone());
        Ok(App {
            close_to_tray: true,
            capturing_keys: false,
            updater,
            net,
            address: String::new(),
            show_pw: false,
            error: None,
            pending: None,
            session: None,
            settings: None,
            confirm_delete: None,
            rename: None,
            thumbs: HashMap::new(),
            gradients: HashMap::new(),
            logo: logo_texture(&cc.egui_ctx),
            #[cfg(debug_assertions)]
            preview: None,
            #[cfg(debug_assertions)]
            font_test: false,
        })
    }

    /// Debug-only: renders the UI with demo data in an off-screen window, saves a screenshot to
    /// `out` and exits. Lets the design be checked without touching the user's desktop.
    #[cfg(debug_assertions)]
    pub fn new_preview(cc: &eframe::CreationContext<'_>, out: String, mode: &str) -> App {
        setup_style(&cc.egui_ctx);
        let mut cfg = crate::config::Config::fresh();
        let demo = [("ฝ่ายบัญชี-PC01", "1610852472"), ("Server-Room", "4506757970"), ("พี่เอ็ม Notebook", "9618864830")];
        let ids: Vec<String> = (0..3).map(|_| iroh::SecretKey::generate().public().to_string()).collect();
        for ((name, short), id) in demo.iter().zip(&ids) {
            cfg.contacts.push(Contact { id: id.clone(), short_id: (*short).into(), name: (*name).into(), ..Default::default() });
        }
        let net = Net::start(cfg, cc.egui_ctx.clone()).expect("start net");
        {
            let mut sh = net.shared.lock().unwrap();
            sh.presence.insert(ids[0].clone(), Presence::Online { name: demo[0].0.into() });
            sh.presence.insert(ids[1].clone(), Presence::Offline);
        }
        let updater = Updater::start(net.runtime(), cc.egui_ctx.clone());
        *updater.state.lock().unwrap() =
            UpdateState::Available { version: "1.1.0".into(), notes: "ตัวอย่าง".into() };
        let mut app = App {
            close_to_tray: false,
            capturing_keys: false,
            updater,
            net,
            address: String::new(),
            show_pw: false,
            error: None,
            pending: None,
            session: None,
            settings: None,
            confirm_delete: None,
            rename: None,
            thumbs: HashMap::new(),
            gradients: HashMap::new(),
            logo: logo_texture(&cc.egui_ctx),
            preview: Some((out, Instant::now())),
            font_test: mode == "fonts",
        };
        match mode {
            "password" => {
                app.pending = Some(Pending {
                    target: ids[2].clone(),
                    label: "พี่เอ็ม Notebook".into(),
                    password: "abc".into(),
                    remember: true,
                    error: Some("รหัสผ่านไม่ถูกต้อง ลองใหม่อีกครั้ง".into()),
                    focus: false,
                })
            }
            "settings" => app.settings = Some(SettingsDraft { name: "DESKTOP-Q4221RT".into(), password: "k7m2p9xa".into(), ..Default::default() }),
            _ => {}
        }
        app
    }

    /// Starts connecting to whatever is in the address bar or on a card.
    fn request_connect(&mut self, target: String, label: String, saved_password: Option<String>) {
        let target = target.trim().to_string();
        if target.is_empty() {
            self.error = Some("กรุณาใส่ ID ของเครื่องปลายทาง".into());
            return;
        }
        {
            let sh = self.net.shared.lock().unwrap();
            let mine = shortid::parse_short(&target);
            if target == sh.my_id || (mine.is_some() && mine == sh.short_id) {
                self.error = Some("นี่คือ ID ของเครื่องนี้เอง".into());
                return;
            }
        }
        self.error = None;
        match saved_password.filter(|p| !p.is_empty()) {
            Some(pw) => self.start_session(target, label, pw, true),
            None => {
                self.pending =
                    Some(Pending { target, label, password: String::new(), remember: true, error: None, focus: true })
            }
        }
    }

    fn start_session(&mut self, target: String, label: String, password: String, remember: bool) {
        let viewer = self.net.connect(target, password.clone());
        self.session = Some(Session {
            viewer,
            label,
            password,
            remember,
            texture: None,
            fb: Vec::new(),
            fb_size: [0, 0],
            scroll: Vec2::ZERO,
            saved_contact: false,
            fullscreen: false,
            stats_at: Instant::now(),
            stats_bytes: 0,
            stats_frames: 0,
            fps: 0.0,
            kbps: 0.0,
            sent_mods: egui::Modifiers::default(),
        });
    }

    /// Remembers a peer after a successful login so it shows up as a card.
    fn remember_contact(&self, id: &str, short: &str, name: &str, password: Option<&str>) {
        let mut sh = self.net.shared.lock().unwrap();
        let existing = sh.cfg.contacts.iter().position(|c| c.id == id);
        let mut c = match existing {
            Some(i) => sh.cfg.contacts.remove(i),
            None => Contact { id: id.into(), ..Default::default() },
        };
        c.name = name.into();
        if !short.is_empty() {
            c.short_id = short.into();
        }
        if let Some(pw) = password {
            c.password = pw.into();
        }
        // Most recent first.
        sh.cfg.contacts.insert(0, c);
        sh.presence.insert(id.to_string(), Presence::Online { name: name.into() });
        sh.cfg.save();
    }

    // ---------------------------------------------------------------- home screen

    fn home_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("topbar")
            .exact_size(60.0)
            .frame(egui::Frame::NONE.fill(SURFACE).inner_margin(egui::Margin::symmetric(16, 0)))
            .show(ui, |ui| self.top_bar(ui));

        self.update_banner(ui);

        let incoming: Vec<(String, iroh::endpoint::Connection)> =
            self.net.shared.lock().unwrap().incoming.iter().map(|i| (i.name.clone(), i.conn.clone())).collect();
        if !incoming.is_empty() {
            egui::Panel::top("incoming")
                .frame(egui::Frame::NONE.fill(Color32::from_rgb(90, 28, 32)).inner_margin(egui::Margin::symmetric(16, 8)))
                .show(ui, |ui| {
                    for (name, conn) in incoming.clone() {
                        ui.horizontal(|ui| {
                            dot(ui, DANGER, 10.0);
                            ui.label(RichText::new(format!("เครื่องนี้กำลังถูกควบคุมโดย {name}")).font(bold(14.0)));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if danger_button(ui, "ตัดการเชื่อมต่อ").clicked() {
                                    conn.close(0u32.into(), b"kicked");
                                }
                            });
                        });
                    }
                });
        }

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(BG)).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                let pad = if ui.available_width() < 720.0 { 14 } else { 32 };
                egui::Frame::NONE.inner_margin(egui::Margin::symmetric(pad, 24)).show(ui, |ui| {
                    self.hero(ui);
                    ui.add_space(28.0);
                    self.cards(ui);
                });
            });
        });

        if !incoming.is_empty() {
            let dropped = dropped_paths(&ctx);
            if !dropped.is_empty() {
                for (name, conn) in &incoming {
                    self.net.send_files(conn.clone(), name.clone(), dropped.clone());
                }
            }
            drop_overlay(&ctx, "วางเพื่อส่งไฟล์ให้เครื่องที่กำลังควบคุม");
        }

        self.password_dialog(&ctx);
        self.settings_dialog(&ctx);
        self.delete_dialog(&ctx);
        self.rename_dialog(&ctx);
    }

    fn rename_dialog(&mut self, ctx: &egui::Context) {
        let Some((id, draft, focus)) = &mut self.rename else { return };
        let mut save = false;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::unique("rename")).frame(dialog_frame()).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.label(RichText::new("แก้ชื่อเครื่อง").font(bold(18.0)));
            ui.label(RichText::new("ตั้งชื่อให้จำง่าย เช่น ชื่อเจ้าของหรือแผนก (เห็นเฉพาะเครื่องนี้)").color(MUTED).size(13.0));
            ui.add_space(10.0);
            let r = ui.add(
                egui::TextEdit::singleline(draft)
                    .hint_text("เว้นว่างเพื่อใช้ชื่อคอมพิวเตอร์")
                    .margin(vec2(10.0, 8.0))
                    .desired_width(f32::INFINITY),
            );
            if std::mem::take(focus) {
                r.request_focus();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                save = true;
            }
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                save |= accent_button(ui, "บันทึก").clicked();
                close = ui.add(egui::Button::new("ยกเลิก").min_size(vec2(0.0, 34.0))).clicked();
            });
        });
        if save {
            let mut sh = self.net.shared.lock().unwrap();
            if let Some(c) = sh.cfg.contacts.iter_mut().find(|c| &c.id == id) {
                c.alias = draft.trim().to_string();
            }
            sh.cfg.save();
        }
        if save || close || modal.should_close() {
            self.rename = None;
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let compact = ui.available_width() < 720.0;
        ui.horizontal_centered(|ui| {
            ui.add(egui::Image::from_texture((self.logo.id(), vec2(30.0, 30.0))));
            if !compact {
                ui.label(RichText::new("RemoteDesk").font(bold(17.0)).color(TEXT));
                ui.label(RichText::new(format!("v{}", updater::VERSION)).color(MUTED).size(12.0));
            }
            ui.add_space(if compact { 8.0 } else { 24.0 });
            // Leave room for the connect button, status and settings on the right.
            let reserved = if compact { 210.0 } else { 330.0 };

            let edit = egui::TextEdit::singleline(&mut self.address)
                .hint_text("ใส่ ID เครื่องปลายทาง เช่น 1 234 567 890")
                .font(FontId::proportional(16.0))
                .margin(vec2(12.0, 8.0))
                .desired_width((ui.available_width() - reserved).clamp(120.0, 520.0));
            let resp = ui.add(edit);
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if accent_button(ui, "เชื่อมต่อ").clicked() || enter {
                let target = self.address.clone();
                let saved = self.saved_password_for(&target);
                self.request_connect(target.clone(), target, saved);
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(egui::Button::new("ตั้งค่า").min_size(vec2(0.0, 34.0))).clicked() {
                    let sh = self.net.shared.lock().unwrap();
                    self.settings =
                        Some(SettingsDraft::from_cfg(&sh.cfg));
                }
                ui.add_space(8.0);
                let online = self.net.shared.lock().unwrap().online;
                if !compact {
                    ui.label(RichText::new(if online { "ออนไลน์" } else { "กำลังเชื่อมต่อ..." }).color(MUTED));
                }
                dot(ui, if online { GREEN } else { AMBER }, 10.0);
            });
        });
    }

    fn saved_password_for(&self, target: &str) -> Option<String> {
        let digits = shortid::parse_short(target);
        let sh = self.net.shared.lock().unwrap();
        sh.cfg
            .contacts
            .iter()
            .find(|c| c.id == target.trim() || (digits.is_some() && digits.as_deref() == Some(c.short_id.as_str())))
            .map(|c| c.password.clone())
    }

    fn hero(&mut self, ui: &mut egui::Ui) {
        let (short, password) = {
            let sh = self.net.shared.lock().unwrap();
            (sh.short_id.clone(), sh.cfg.password.clone())
        };
        egui::Frame::NONE
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(14)
            .inner_margin(egui::Margin::symmetric(28, 22))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let narrow = ui.available_width() < 640.0;
                let body = |ui: &mut egui::Ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("ID ของเครื่องนี้").color(MUTED).size(14.0));
                        match &short {
                            Some(s) => {
                                let r = ui.add(
                                    egui::Label::new(RichText::new(shortid::format(s)).font(bold(46.0)).color(ID_COLOR))
                                        .sense(Sense::click()),
                                );
                                if r.on_hover_text("คลิกเพื่อคัดลอก").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                    ui.ctx().copy_text(shortid::format(s));
                                }
                            }
                            None => {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(RichText::new("กำลังสร้าง ID...").size(20.0).color(MUTED));
                                });
                            }
                        }
                        if !crate::localinput::is_elevated() {
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                dot(ui, AMBER, 8.0);
                                ui.label(RichText::new("สิทธิ์จำกัด").color(AMBER).size(12.5)).on_hover_text(
                                    "คนที่เข้ามาคุมจะคลิกหรือพิมพ์ในโปรแกรมที่รันแบบผู้ดูแลระบบไม่ได้ เช่น Task Manager หรือตัวติดตั้งโปรแกรม",
                                );
                                if ui.small_button("เปิดแบบผู้ดูแลระบบ").clicked() && crate::localinput::relaunch_as_admin() {
                                    std::process::exit(0);
                                }
                            });
                        }
                        ui.horizontal(|ui| {
                            if let Some(s) = &short {
                                if ui.add(egui::Button::new("คัดลอก ID").min_size(vec2(0.0, 28.0))).clicked() {
                                    ui.ctx().copy_text(shortid::format(s));
                                }
                            }
                            ui.label(RichText::new("ส่ง ID และรหัสผ่านให้คนที่จะเข้ามาควบคุม").color(MUTED).size(13.0));
                        });
                    });
                    let side = if narrow { Layout::top_down(Align::Min) } else { Layout::right_to_left(Align::Center) };
                    if narrow {
                        ui.add_space(12.0);
                    }
                    ui.with_layout(side, |ui| {
                        egui::Frame::NONE
                            .fill(SURFACE2)
                            .corner_radius(10)
                            .inner_margin(egui::Margin::symmetric(16, 12))
                            .show(ui, |ui| {
                                ui.set_width(if narrow { ui.available_width() } else { 300.0 });
                                ui.vertical(|ui| {
                                    ui.label(RichText::new("รหัสผ่าน").color(MUTED).size(13.0));
                                    let shown = if self.show_pw { password.clone() } else { "•".repeat(password.chars().count()) };
                                    ui.label(RichText::new(shown).font(FontId::monospace(22.0)).color(TEXT));
                                    ui.horizontal(|ui| {
                                        if ui.small_button(if self.show_pw { "ซ่อน" } else { "แสดง" }).clicked() {
                                            self.show_pw = !self.show_pw;
                                        }
                                        if ui.small_button("คัดลอก").clicked() {
                                            ui.ctx().copy_text(password.clone());
                                        }
                                        if ui.small_button("ตั้งเอง").clicked() {
                                            let sh = self.net.shared.lock().unwrap();
                                            self.settings = Some(SettingsDraft::from_cfg(&sh.cfg));
                                        }
                                        if ui.small_button("สุ่มใหม่").clicked() {
                                            let mut sh = self.net.shared.lock().unwrap();
                                            sh.cfg.password = random_password();
                                            sh.cfg.save();
                                        }
                                    });
                                });
                            });
                    });
                };
                if narrow {
                    ui.vertical(body);
                } else {
                    ui.horizontal(body);
                }
            });
        if let Some(err) = &self.error {
            ui.add_space(10.0);
            egui::Frame::NONE
                .fill(Color32::from_rgb(66, 26, 34))
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(14, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(err).color(DANGER_TEXT));
                });
        }
    }

    fn cards(&mut self, ui: &mut egui::Ui) {
        let (contacts, presence) = {
            let sh = self.net.shared.lock().unwrap();
            (sh.cfg.contacts.clone(), sh.presence.clone())
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new("เครื่องล่าสุด").font(bold(16.0)).color(TEXT));
            ui.label(RichText::new(format!("{}", contacts.len())).color(MUTED));
        });
        ui.add_space(4.0);
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().rect_filled(r, 0.0, BORDER);
        ui.add_space(14.0);

        if contacts.is_empty() {
            egui::Frame::NONE
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(12)
                .inner_margin(egui::Margin::same(28))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("ยังไม่มีเครื่องในรายการ").font(bold(15.0)).color(TEXT));
                        ui.label(RichText::new("ใส่ ID ที่ช่องด้านบนแล้วกดเชื่อมต่อ เครื่องที่เชื่อมสำเร็จจะแสดงที่นี่").color(MUTED));
                    });
                });
            return;
        }

        let mut connect = None;
        // As many columns as fit; stretch cards so the row fills the width.
        let gap = 14.0;
        let avail = ui.available_width();
        let cols = ((avail + gap) / (CARD.x + gap)).floor().max(1.0);
        let w = ((avail - gap * (cols - 1.0)) / cols).min(340.0);
        let card_size = vec2(w, w * CARD.y / CARD.x);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(gap, gap);
            for c in &contacts {
                let p = presence.get(&c.id).cloned();
                if let Some(action) = self.card(ui, c, p, card_size) {
                    match action {
                        CardAction::Connect => connect = Some(c.clone()),
                        CardAction::Delete => self.confirm_delete = Some(c.clone()),
                        CardAction::Rename => self.rename = Some((c.id.clone(), c.display_name(None), true)),
                    }
                }
            }
        });
        if let Some(c) = connect {
            let name = c.display_name(None);
            let label = if name.is_empty() { shortid::format(&c.short_id) } else { name };
            self.request_connect(c.id.clone(), label, Some(c.password.clone()));
        }
    }

    fn card(&mut self, ui: &mut egui::Ui, c: &Contact, presence: Option<Presence>, size: Vec2) -> Option<CardAction> {
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        let painter = ui.painter_at(rect.expand(2.0));
        let radius = CornerRadius::same(12);

        let (status_color, status_text, name) = match &presence {
            Some(Presence::Online { name }) => (GREEN, "ออนไลน์", c.display_name(Some(name))),
            Some(Presence::Offline) => (Color32::from_rgb(120, 120, 128), "ออฟไลน์", c.display_name(None)),
            None => (AMBER, "กำลังตรวจสอบ", c.display_name(None)),
        };
        let offline = matches!(presence, Some(Presence::Offline));

        // Background: last screenshot of that machine, or a gradient unique to its ID.
        let tint = if offline { Color32::from_gray(150) } else { Color32::WHITE };
        if let Some(tex) = self.thumb(ui.ctx(), &c.id) {
            let [tw, th] = tex.size();
            let uv = cover_uv(tw as f32 / th as f32, size.x / size.y);
            egui::Image::from_texture((tex.id(), size)).uv(uv).corner_radius(radius).tint(tint).paint_at(ui, rect);
        } else {
            let tex = self.gradient(ui.ctx(), &c.id);
            egui::Image::from_texture((tex.id(), size)).corner_radius(radius).tint(tint).paint_at(ui, rect);
        }

        // Bottom caption strip.
        let strip = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 52.0), rect.max);
        painter.rect_filled(strip, CornerRadius { nw: 0, ne: 0, sw: 12, se: 12 }, Color32::from_black_alpha(175));
        monitor_icon(&painter, pos2(strip.left() + 22.0, strip.center().y), TEXT);
        let title = if name.is_empty() { "ไม่มีชื่อ".to_string() } else { name };
        painter.text(pos2(strip.left() + 42.0, strip.top() + 8.0), Align2::LEFT_TOP, &title, bold(14.5), TEXT);
        let sub = if c.short_id.is_empty() { format!("{}…", &c.id[..12.min(c.id.len())]) } else { shortid::format(&c.short_id) };
        painter.text(
            pos2(strip.left() + 42.0, strip.bottom() - 8.0),
            Align2::LEFT_BOTTOM,
            format!("{sub}  ·  {status_text}"),
            FontId::proportional(12.5),
            Color32::from_gray(200),
        );

        // Status dot.
        let dot_c = pos2(rect.left() + 16.0, rect.top() + 16.0);
        painter.circle_filled(dot_c, 7.5, Color32::from_black_alpha(140));
        painter.circle_filled(dot_c, 5.5, status_color);

        // Delete button (top-right).
        let del_rect = Rect::from_center_size(pos2(rect.right() - 18.0, rect.top() + 18.0), vec2(26.0, 26.0));
        let del = ui.interact(del_rect, resp.id.with("del"), Sense::click()).on_hover_text("ลบออกจากรายการ");
        if resp.hovered() || del.hovered() {
            let fill = if del.hovered() { DANGER } else { Color32::from_black_alpha(150) };
            painter.circle_filled(del_rect.center(), 12.0, fill);
            let d = 4.5;
            let c0 = del_rect.center();
            let s = Stroke::new(1.8, Color32::WHITE);
            painter.line_segment([c0 + vec2(-d, -d), c0 + vec2(d, d)], s);
            painter.line_segment([c0 + vec2(-d, d), c0 + vec2(d, -d)], s);
        }

        // Rename button, left of delete.
        let ren_rect = del_rect.translate(vec2(-30.0, 0.0));
        let ren = ui.interact(ren_rect, resp.id.with("ren"), Sense::click()).on_hover_text("แก้ชื่อเครื่อง");
        if resp.hovered() || del.hovered() || ren.hovered() {
            let fill = if ren.hovered() { ACCENT } else { Color32::from_black_alpha(150) };
            painter.circle_filled(ren_rect.center(), 12.0, fill);
            // Pencil.
            let c0 = ren_rect.center();
            let s = Stroke::new(1.8, Color32::WHITE);
            painter.line_segment([c0 + vec2(-4.5, 4.5), c0 + vec2(4.0, -4.0)], s);
            painter.line_segment([c0 + vec2(-5.5, 5.5), c0 + vec2(-3.0, 5.5)], s);
        }
        if ren.clicked() {
            return Some(CardAction::Rename);
        }

        let border = if resp.hovered() { Stroke::new(2.0, ACCENT_LIGHT) } else { Stroke::new(1.0, BORDER) };
        painter.rect_stroke(rect, radius, border, egui::StrokeKind::Inside);

        if del.clicked() {
            Some(CardAction::Delete)
        } else if resp.clicked() {
            Some(CardAction::Connect)
        } else {
            None
        }
    }

    fn thumb(&mut self, ctx: &egui::Context, id: &str) -> Option<TextureHandle> {
        self.thumbs
            .entry(id.to_string())
            .or_insert_with(|| {
                let bytes = std::fs::read(thumb_path(id)).ok()?;
                let opts = zune_jpeg::zune_core::options::DecoderOptions::default()
                    .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGBA);
                let mut d = zune_jpeg::JpegDecoder::new_with_options(zune_jpeg::zune_core::bytestream::ZCursor::new(&bytes[..]), opts);
                let rgba = d.decode().ok()?;
                let (w, h) = d.dimensions()?;
                let img = ColorImage::from_rgba_unmultiplied([w, h], &rgba);
                Some(ctx.load_texture(format!("thumb-{id}"), img, TextureOptions::LINEAR))
            })
            .clone()
    }

    fn gradient(&mut self, ctx: &egui::Context, id: &str) -> TextureHandle {
        self.gradients
            .entry(id.to_string())
            .or_insert_with(|| {
                let seed = id.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
                // Stay within the logo's indigo-violet range so cards match the brand.
                let hue = 0.66 + (seed % 80) as f32 / 1000.0;
                let c = |h: f32, s: f32, v: f32| {
                    let rgb = egui::ecolor::Hsva::new(h.rem_euclid(1.0), s, v, 1.0).to_srgb();
                    [rgb[0], rgb[1], rgb[2], 255]
                };
                let px = [c(hue, 0.8, 0.78), c(hue + 0.04, 0.7, 0.5), c(hue - 0.03, 0.85, 0.42), c(hue + 0.02, 0.75, 0.18)];
                let img = ColorImage::from_rgba_unmultiplied([2, 2], px.as_flattened());
                ctx.load_texture(format!("grad-{id}"), img, TextureOptions::LINEAR)
            })
            .clone()
    }

    // ---------------------------------------------------------------- dialogs

    fn password_dialog(&mut self, ctx: &egui::Context) {
        let Some(p) = &mut self.pending else { return };
        let mut go = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::unique("password")).frame(dialog_frame()).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.label(RichText::new("ใส่รหัสผ่าน").font(bold(18.0)));
            ui.label(RichText::new(format!("เชื่อมต่อไปยัง {}", p.label)).color(MUTED));
            ui.add_space(12.0);
            let r = ui.add(
                egui::TextEdit::singleline(&mut p.password)
                    .password(true)
                    .hint_text("รหัสผ่านของเครื่องปลายทาง")
                    .font(FontId::proportional(16.0))
                    .margin(vec2(10.0, 8.0))
                    .desired_width(f32::INFINITY),
            );
            if p.focus {
                r.request_focus();
                p.focus = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                go = true;
            }
            if let Some(err) = &p.error {
                ui.colored_label(DANGER_TEXT, err);
            }
            ui.add_space(6.0);
            ui.checkbox(&mut p.remember, "จำรหัสผ่านไว้สำหรับครั้งต่อไป");
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if accent_button(ui, "เชื่อมต่อ").clicked() {
                    go = true;
                }
                if ui.add(egui::Button::new("ยกเลิก").min_size(vec2(0.0, 34.0))).clicked() {
                    cancel = true;
                }
            });
        });
        if modal.should_close() {
            cancel = true;
        }
        if go && !p.password.is_empty() {
            let p = self.pending.take().unwrap();
            self.start_session(p.target, p.label, p.password, p.remember);
        } else if cancel {
            self.pending = None;
        }
    }

    fn settings_dialog(&mut self, ctx: &egui::Context) {
        let Some(draft) = &mut self.settings else { return };
        let (my_id, short) = {
            let sh = self.net.shared.lock().unwrap();
            (sh.my_id.clone(), sh.short_id.clone().unwrap_or_default())
        };
        let mut save = false;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::unique("settings")).frame(dialog_frame()).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new("ตั้งค่า").font(bold(18.0)));
            ui.add_space(12.0);
            ui.label(RichText::new("ชื่อเครื่อง (แสดงให้อีกฝั่งเห็น)").color(MUTED));
            ui.add(egui::TextEdit::singleline(&mut draft.name).margin(vec2(10.0, 7.0)).desired_width(f32::INFINITY));
            ui.add_space(8.0);
            ui.label(RichText::new("รหัสผ่านสำหรับให้คนอื่นเข้าเครื่องนี้").color(MUTED));
            ui.add(egui::TextEdit::singleline(&mut draft.password).margin(vec2(10.0, 7.0)).desired_width(f32::INFINITY));
            if draft.password.chars().count() < 4 {
                ui.colored_label(AMBER, "รหัสผ่านต้องมีอย่างน้อย 4 ตัวอักษร");
            }
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);
            ui.checkbox(&mut draft.start_with_windows, "เปิดโปรแกรมอัตโนมัติเมื่อเปิด Windows");
            ui.checkbox(&mut draft.ask_on_connect, "ถามก่อนทุกครั้งที่มีคนขอเข้าควบคุม");
            ui.label(RichText::new("ถ้าเปิด ต้องมีคนที่หน้าเครื่องกดอนุญาต การเข้าแบบไม่มีคนดูแลจะใช้ไม่ได้").color(MUTED).size(11.5));
            ui.add_space(6.0);
            if ui.link(RichText::new("เปิดบันทึกการเข้าใช้งาน (access.log)").size(12.5)).clicked() {
                let path = crate::config::config_dir().join("access.log");
                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&path);
                let _ = std::process::Command::new("notepad").arg(path).spawn();
            }
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(RichText::new(format!("เวอร์ชัน {}", updater::VERSION)).color(MUTED).size(12.0));
            ui.add_space(4.0);
            ui.label(RichText::new("ID ถาวรของเครื่องนี้").color(MUTED));
            if short.is_empty() {
                ui.label(RichText::new("กำลังสร้าง ID...").color(MUTED));
            } else {
                ui.label(RichText::new(shortid::format(&short)).font(bold(16.0)));
            }
            ui.label(RichText::new("ผูกกับ Windows ของเครื่องนี้ ไม่เปลี่ยนแม้ติดตั้งโปรแกรมใหม่").color(MUTED).size(12.0));
            egui::CollapsingHeader::new(RichText::new("ID แบบเต็ม (ขั้นสูง)").color(MUTED).size(12.0)).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(&my_id).monospace().size(11.0)).wrap());
                if ui.small_button("คัดลอก").clicked() {
                    ui.ctx().copy_text(my_id.clone());
                }
            });
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if accent_button(ui, "บันทึก").clicked() && draft.password.chars().count() >= 4 {
                    save = true;
                }
                if ui.add(egui::Button::new("ยกเลิก").min_size(vec2(0.0, 34.0))).clicked() {
                    close = true;
                }
            });
        });
        if save {
            let start = draft.start_with_windows;
            let mut sh = self.net.shared.lock().unwrap();
            sh.cfg.device_name = draft.name.trim().to_string();
            sh.cfg.password = draft.password.clone();
            sh.cfg.ask_on_connect = draft.ask_on_connect;
            sh.cfg.start_with_windows = start;
            sh.cfg.save();
            drop(sh);
            crate::autostart::set_enabled(start);
        }
        if save || close || modal.should_close() {
            self.settings = None;
        }
    }

    fn delete_dialog(&mut self, ctx: &egui::Context) {
        let Some(c) = &self.confirm_delete else { return };
        let mut delete = false;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::unique("delete")).frame(dialog_frame()).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.label(RichText::new("ลบเครื่องนี้ออกจากรายการ?").font(bold(17.0)));
            ui.label(RichText::new(format!("{}  {}", c.name, shortid::format(&c.short_id))).color(MUTED));
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                delete = danger_button(ui, "ลบ").clicked();
                close = ui.add(egui::Button::new("ยกเลิก").min_size(vec2(0.0, 34.0))).clicked();
            });
        });
        if delete {
            let id = c.id.clone();
            let mut sh = self.net.shared.lock().unwrap();
            sh.cfg.contacts.retain(|c| c.id != id);
            sh.presence.remove(&id);
            sh.cfg.save();
            let _ = std::fs::remove_file(thumb_path(&id));
            self.thumbs.remove(&id);
        }
        if delete || close || modal.should_close() {
            self.confirm_delete = None;
        }
    }

    // ---------------------------------------------------------------- remote session

    fn connecting_ui(&mut self, ui: &mut egui::Ui) {
        let Some(s) = &self.session else { return };
        let label = s.label.clone();
        self.home_ui(ui);
        let mut cancel = false;
        egui::Modal::new(egui::Id::unique("connecting")).frame(dialog_frame()).show(ui.ctx(), |ui| {
            ui.set_width(320.0);
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);
                ui.add(egui::Spinner::new().size(34.0).color(ACCENT));
                ui.add_space(10.0);
                ui.label(RichText::new("กำลังเชื่อมต่อ").font(bold(17.0)));
                ui.label(RichText::new(label).color(MUTED));
                ui.add_space(12.0);
                cancel = ui.add(egui::Button::new("ยกเลิก").min_size(vec2(100.0, 32.0))).clicked();
            });
        });
        if cancel {
            if let Some(s) = self.session.take() {
                s.viewer.lock().unwrap().disconnect();
            }
        }
    }

    fn end_session(&mut self, ctx: &egui::Context) {
        let Some(s) = self.session.take() else { return };
        if s.fullscreen {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        let id = s.viewer.lock().unwrap().peer_id.clone();
        if s.fb_size[0] > 0 && s.saved_contact {
            save_thumbnail(&id, &s.fb, s.fb_size);
            self.thumbs.remove(&id);
        }
    }

    fn session_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let Some(s) = &mut self.session else { return };
        let remote_busy = s.viewer.lock().unwrap().remote_busy;
        let (status, peer_id, peer_short, name, direct, frames, bytes, patches) = {
            let mut v = s.viewer.lock().unwrap();
            (
                v.status.clone(),
                v.peer_id.clone(),
                v.peer_short.clone(),
                v.peer_name.clone(),
                v.direct,
                v.frames,
                v.bytes,
                std::mem::take(&mut v.patches),
            )
        };

        match status {
            ViewStatus::Failed(msg) => {
                let s = self.session.take().unwrap();
                if msg == WRONG_PASSWORD {
                    let target = s.viewer.lock().unwrap().peer_id.clone();
                    self.pending = Some(Pending {
                        target,
                        label: s.label,
                        password: String::new(),
                        remember: s.remember,
                        error: Some("รหัสผ่านไม่ถูกต้อง ลองใหม่อีกครั้ง".into()),
                        focus: true,
                    });
                } else {
                    self.error = Some(msg);
                }
                self.home_ui(ui);
                return;
            }
            ViewStatus::Closed => {
                self.end_session(&ctx);
                self.home_ui(ui);
                return;
            }
            ViewStatus::Connecting => {
                self.connecting_ui(ui);
                return;
            }
            ViewStatus::Connected => {}
        }

        if !s.saved_contact && !name.is_empty() {
            s.saved_contact = true;
            let pw = s.remember.then(|| s.password.clone());
            let (id, short, nm) = (peer_id.clone(), peer_short.clone(), name.clone());
            self.remember_contact(&id, &short, &nm, pw.as_deref());
            self.address.clear();
        }
        let Some(s) = &mut self.session else { return };

        // Apply received screen updates to the texture and the CPU copy.
        for p in patches {
            if s.texture.is_none() || s.fb_size != p.screen {
                let blank = ColorImage::from_rgba_unmultiplied(p.screen, &vec![0; p.screen[0] * p.screen[1] * 4]);
                s.texture = Some(ctx.load_texture("remote", blank, TextureOptions::LINEAR));
                s.fb = vec![0; p.screen[0] * p.screen[1] * 4];
                s.fb_size = p.screen;
            }
            if p.pos[0] + p.size[0] <= p.screen[0] && p.pos[1] + p.size[1] <= p.screen[1] {
                for row in 0..p.size[1] {
                    let dst = ((p.pos[1] + row) * p.screen[0] + p.pos[0]) * 4;
                    let src = row * p.size[0] * 4;
                    s.fb[dst..dst + p.size[0] * 4].copy_from_slice(&p.rgba[src..src + p.size[0] * 4]);
                }
                let img = ColorImage::from_rgba_unmultiplied(p.size, &p.rgba);
                s.texture.as_mut().unwrap().set_partial(p.pos, img, TextureOptions::LINEAR);
            }
        }

        let elapsed = s.stats_at.elapsed().as_secs_f32();
        if elapsed >= 1.0 {
            s.fps = (frames - s.stats_frames) as f32 / elapsed;
            s.kbps = (bytes - s.stats_bytes) as f32 / 1024.0 / elapsed;
            s.stats_at = Instant::now();
            s.stats_frames = frames;
            s.stats_bytes = bytes;
        }

        let mut disconnect = false;
        let mut send_files: Vec<std::path::PathBuf> = Vec::new();
        egui::Panel::top("toolbar")
            .exact_size(46.0)
            .frame(egui::Frame::NONE.fill(SURFACE).inner_margin(egui::Margin::symmetric(14, 0)))
            .show(ui, |ui| {
                let wide = ui.available_width() > 900.0;
                ui.horizontal_centered(|ui| {
                    let (icon, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                    monitor_icon(ui.painter(), icon.center(), TEXT);
                    ui.label(RichText::new(&name).font(bold(15.0)));
                    if !peer_short.is_empty() {
                        ui.label(RichText::new(shortid::format(&peer_short)).color(MUTED));
                    }
                    ui.add_space(10.0);
                    let (c, t) = if direct { (GREEN, "เชื่อมต่อตรง") } else { (AMBER, "ผ่าน Relay") };
                    pill(ui, c, t);
                    if wide && crate::localinput::key_capture_active() {
                        ui.label(RichText::new("คีย์ลัดทั้งหมดส่งไปเครื่องปลายทาง").color(MUTED).size(12.5))
                            .on_hover_text("รวมถึงปุ่ม Win และ Alt+Tab คลิกนอกหน้าต่างเพื่อใช้คีย์บอร์ดกับเครื่องตัวเอง");
                    }
                    if remote_busy {
                        pill(ui, AMBER, "ปลายทางกำลังใช้เมาส์/คีย์บอร์ดเอง");
                    }
                    if wide {
                        ui.label(RichText::new(format!("{:.0} fps · {:.0} KB/s", s.fps, s.kbps)).color(MUTED).size(13.0));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if danger_button(ui, "ตัดการเชื่อมต่อ").clicked() {
                            disconnect = true;
                        }
                        let fs = if s.fullscreen { "ออกจากเต็มจอ" } else { "เต็มจอ" };
                        if ui.add(egui::Button::new(fs).min_size(vec2(0.0, 32.0))).clicked() {
                            s.fullscreen = !s.fullscreen;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(s.fullscreen));
                        }
                    });
                });
            });
        send_files.extend(dropped_paths(&ctx));
        if !send_files.is_empty() {
            let conn = s.viewer.lock().unwrap().connection();
            if let Some(conn) = conn {
                self.net.send_files(conn, name.clone(), send_files);
            }
        }
        drop_overlay(&ctx, "วางเพื่อส่งไฟล์ไปยังเครื่องปลายทาง");
        let Some(s) = &mut self.session else { return };

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(Color32::BLACK)).show(ui, |ui| {
            let avail = ui.available_rect_before_wrap();
            let Some(tex) = &s.texture else {
                ui.centered_and_justified(|ui| ui.add(egui::Spinner::new().size(34.0).color(ACCENT)));
                return;
            };
            let [tw, th] = s.fb_size;
            let scale = (avail.width() / tw as f32).min(avail.height() / th as f32);
            let rect = Rect::from_center_size(avail.center(), vec2(tw as f32 * scale, th as f32 * scale));
            ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            let response = ui.allocate_rect(rect, Sense::click_and_drag());
            let hovered = response.hovered() || response.dragged();

            let viewer = s.viewer.lock().unwrap();
            let (events, cur_mods) = ui.input(|i| (i.events.clone(), i.modifiers));
            for event in events {
                match event {
                    egui::Event::PointerMoved(pos) if hovered => {
                        let p = (pos - rect.min) / rect.size();
                        viewer.send(Input::MouseMove { x: p.x, y: p.y });
                    }
                    egui::Event::PointerButton { pos, button, pressed, .. } if !pressed || rect.contains(pos) => {
                        let button = match button {
                            egui::PointerButton::Secondary => 1,
                            egui::PointerButton::Middle => 2,
                            _ => 0,
                        };
                        let p = (pos - rect.min) / rect.size();
                        viewer.send(Input::MouseMove { x: p.x, y: p.y });
                        viewer.send(Input::Button { button, down: pressed });
                    }
                    egui::Event::MouseWheel { unit, delta, .. } if hovered => {
                        let lines = match unit {
                            egui::MouseWheelUnit::Point => delta / 40.0,
                            egui::MouseWheelUnit::Line => delta,
                            egui::MouseWheelUnit::Page => delta * 10.0,
                        };
                        s.scroll += lines;
                        let (dx, dy) = (s.scroll.x.trunc(), s.scroll.y.trunc());
                        if dx != 0.0 || dy != 0.0 {
                            s.scroll -= vec2(dx, dy);
                            viewer.send(Input::Scroll { dx: -dx as i32, dy: -dy as i32 });
                        }
                    }
                    egui::Event::Key { key, physical_key, pressed, modifiers, .. } => {
                        // Keep the remote's modifier keys in sync first, so combos land correctly
                        // and nothing stays stuck when a modifier is released.
                        sync_mods(&viewer, &mut s.sent_mods, modifiers);
                        if let Some(vk) = vk_for(physical_key.unwrap_or(key)) {
                            viewer.send(Input::KeyVk { vk, down: pressed });
                        }
                    }
                    // egui turns Ctrl+C/X/V into these instead of key events; the Ctrl is already
                    // held via sync_mods, so just tap the letter.
                    egui::Event::Copy | egui::Event::Cut => {
                        let vk = if matches!(event, egui::Event::Copy) { b'C' } else { b'X' } as u16;
                        sync_mods(&viewer, &mut s.sent_mods, egui::Modifiers { ctrl: true, ..Default::default() });
                        viewer.send(Input::KeyVk { vk, down: true });
                        viewer.send(Input::KeyVk { vk, down: false });
                    }
                    egui::Event::Paste(_) => {
                        sync_mods(&viewer, &mut s.sent_mods, egui::Modifiers { ctrl: true, ..Default::default() });
                        viewer.send(Input::KeyVk { vk: b'V' as u16, down: true });
                        viewer.send(Input::KeyVk { vk: b'V' as u16, down: false });
                    }
                    _ => {}
                }
            }
            // Final reconcile: release any modifier whose key was let go without an event.
            sync_mods(&viewer, &mut s.sent_mods, cur_mods);
        });

        if disconnect {
            s.viewer.lock().unwrap().disconnect();
        }
    }
}

enum CardAction {
    Connect,
    Delete,
    Rename,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        #[cfg(debug_assertions)]
        if let Some((out, started)) = &self.preview {
            ui.ctx().request_repaint();
            if started.elapsed().as_secs_f32() > 6.0 {
                let out = out.clone();
                self.preview = None;
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                    let mut jpeg = Vec::new();
                    let _ = jpeg_encoder::Encoder::new(&mut jpeg, 92).encode(
                        &rgba,
                        img.size[0] as u16,
                        img.size[1] as u16,
                        jpeg_encoder::ColorType::Rgba,
                    );
                    let _ = std::fs::write(&out, jpeg);
                    std::process::exit(0);
                })));
            }
        }
        #[cfg(debug_assertions)]
        if self.font_test {
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(BG).inner_margin(20)).show(ui, |ui| {
                for size in [12.5, 14.5, 18.0, 46.0] {
                    for font in [FontId::proportional(size), bold(size)] {
                        let r = ui.label(RichText::new("คัดลอก ID ของเครื่องนี้ Hello 123 ทดสอบ").font(font));
                        ui.painter().hline(r.rect.x_range(), r.rect.center().y, Stroke::new(1.0, Color32::from_rgb(255, 0, 0)));
                    }
                }
                let _ = ui.button("คัดลอก ID");
            });
            return;
        }
        // Closing the window hides to the tray; quit from the tray menu to exit.
        if self.close_to_tray && ui.ctx().input(|i| i.viewport().close_requested()) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        if self.session.is_some() {
            self.session_ui(ui);
        } else {
            self.home_ui(ui);
        }
        self.transfers_ui(ui.ctx());
        self.access_request_dialog(ui.ctx());
        self.update_key_capture(ui.ctx());
    }
}

impl App {
    /// Grabs the whole keyboard (Win, Alt+Tab, ...) while a connected session window is focused
    /// and no dialog is open; releases it the moment that stops being true.
    /// Shows the oldest pending "allow this person?" prompt (when ask-on-connect is on).
    fn access_request_dialog(&mut self, ctx: &egui::Context) {
        let pending = {
            let sh = self.net.shared.lock().unwrap();
            sh.requests.first().map(|r| (r.id, r.name.clone(), r.peer.clone()))
        };
        let Some((id, name, peer)) = pending else { return };
        let mut answer: Option<bool> = None;
        egui::Modal::new(egui::Id::unique("access")).frame(dialog_frame()).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.label(RichText::new("มีคนขอเข้าควบคุมเครื่องนี้").font(bold(18.0)));
            ui.add_space(6.0);
            ui.label(RichText::new(&name).font(bold(15.0)));
            ui.label(RichText::new(format!("ID {}…", &peer[..peer.len().min(12)])).color(MUTED).size(12.0));
            ui.add_space(6.0);
            ui.label(RichText::new("อนุญาตให้ควบคุมเครื่องนี้หรือไม่? (รหัสผ่านถูกต้องแล้ว)").color(MUTED).size(13.0));
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if accent_button(ui, "อนุญาต").clicked() {
                    answer = Some(true);
                }
                if danger_button(ui, "ปฏิเสธ").clicked() {
                    answer = Some(false);
                }
            });
        });
        if let Some(allow) = answer {
            let mut sh = self.net.shared.lock().unwrap();
            if let Some(req) = sh.requests.iter_mut().find(|r| r.id == id) {
                req.answer(allow);
            }
        }
    }

    fn update_key_capture(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(false));
        let tx = self.session.as_ref().and_then(|s| {
            let v = s.viewer.lock().unwrap();
            (v.status == ViewStatus::Connected).then(|| v.input.clone()).flatten()
        });
        // Keys come from this window's own input events (egui), which is reliable for Ctrl shortcuts.
        let want = false && focused && tx.is_some() && self.pending.is_none() && self.settings.is_none();
        if want != self.capturing_keys {
            self.capturing_keys = want;
            crate::localinput::set_key_capture(tx.filter(|_| want).map(|tx| {
                Box::new(move |input: Input| {
                    let _ = tx.send(input);
                }) as Box<dyn Fn(Input) + Send>
            }));
        }
    }
}

impl App {
    fn update_banner(&mut self, ui: &mut egui::Ui) {
        let state = self.updater.state.lock().unwrap().clone();
        if state == UpdateState::None {
            return;
        }
        let mut install = false;
        let mut dismiss = false;
        egui::Panel::top("update")
            .frame(egui::Frame::NONE.fill(Color32::from_rgb(40, 30, 78)).inner_margin(egui::Margin::symmetric(16, 8)))
            .show(ui, |ui| {
                ui.horizontal(|ui| match &state {
                    UpdateState::Available { version, .. } => {
                        dot(ui, ACCENT_LIGHT, 10.0);
                        ui.label(RichText::new(format!("มีเวอร์ชันใหม่ {version} (ตอนนี้ {})", updater::VERSION)).font(bold(14.0)));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            dismiss = ui.add(egui::Button::new("ไว้ทีหลัง").min_size(vec2(0.0, 32.0))).clicked();
                            install = accent_button(ui, "อัปเดตเลย").clicked();
                        });
                    }
                    UpdateState::Downloading { version, done, total } => {
                        ui.spinner();
                        ui.label(format!("กำลังดาวน์โหลดเวอร์ชัน {version}"));
                        let frac = if *total > 0 { *done as f32 / *total as f32 } else { 0.0 };
                        ui.add(egui::ProgressBar::new(frac).fill(ACCENT).desired_width(220.0).show_percentage());
                    }
                    UpdateState::Failed(msg) => {
                        ui.colored_label(DANGER_TEXT, msg);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            dismiss = ui.small_button("✕").clicked();
                        });
                    }
                    UpdateState::None => {}
                });
            });
        if install {
            if self.session.is_some() {
                self.error = Some("กรุณาตัดการเชื่อมต่อก่อนอัปเดต".into());
            } else {
                self.updater.install(self.net.runtime(), ui.ctx().clone());
            }
        }
        if dismiss {
            *self.updater.state.lock().unwrap() = UpdateState::None;
        }
    }

    fn transfers_ui(&mut self, ctx: &egui::Context) {
        let transfers = {
            let sh = self.net.shared.lock().unwrap();
            let mut t = sh.transfers.lock().unwrap();
            // Successful transfers disappear 10 s after finishing; failures stay until closed.
            t.retain(|t| !(t.finished && t.error.is_none() && t.finished_at.is_some_and(|f| f.elapsed().as_secs() > 10)));
            t.clone()
        };
        if transfers.is_empty() {
            return;
        }
        if transfers.iter().any(|t| t.finished && t.error.is_none()) {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        let mut dismiss = Vec::new();
        egui::Area::new(egui::Id::unique("transfers"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-16.0, -16.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(SURFACE)
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(12)
                    .inner_margin(egui::Margin::same(14))
                    .shadow(egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(110) })
                    .show(ui, |ui| {
                        ui.set_width(340.0);
                        ui.label(RichText::new("การโอนไฟล์").font(bold(14.0)));
                        for t in transfers.iter().rev().take(5) {
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                let dir = if t.incoming { format!("รับจาก {}", t.peer) } else { format!("ส่งไป {}", t.peer) };
                                ui.label(RichText::new(dir).color(MUTED).size(12.0));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if t.finished && ui.small_button("✕").clicked() {
                                        dismiss.push(t.id);
                                    }
                                });
                            });
                            ui.add(egui::Label::new(RichText::new(&t.name).size(13.5)).truncate());
                            if let Some(err) = &t.error {
                                ui.colored_label(DANGER_TEXT, err);
                            } else if t.finished {
                                ui.horizontal(|ui| {
                                    ui.colored_label(GREEN, if t.incoming { "รับเรียบร้อย" } else { "ส่งเรียบร้อย" });
                                    if t.incoming {
                                        if let Some(path) = &t.path {
                                            if ui.small_button("เปิดโฟลเดอร์").clicked() {
                                                let _ = std::process::Command::new("explorer")
                                                    .arg(format!("/select,{}", path.display()))
                                                    .spawn();
                                            }
                                        }
                                    }
                                });
                            } else {
                                let frac = if t.size > 0 { t.done as f32 / t.size as f32 } else { 0.0 };
                                ui.add(
                                    egui::ProgressBar::new(frac)
                                        .fill(ACCENT)
                                        .text(format!("{} / {}", human_size(t.done), human_size(t.size))),
                                );
                            }
                        }
                    });
            });
        if !dismiss.is_empty() {
            let sh = self.net.shared.lock().unwrap();
            sh.transfers.lock().unwrap().retain(|t| !dismiss.contains(&t.id));
        }
    }
}

fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", b / 1024.0 / 1024.0 / 1024.0)
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    } else {
        format!("{:.0} KB", (b / 1024.0).ceil())
    }
}

fn dropped_paths(ctx: &egui::Context) -> Vec<std::path::PathBuf> {
    ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect())
}

/// Dims the window with a hint while files are being dragged over it.
fn drop_overlay(ctx: &egui::Context, text: &str) {
    if ctx.input(|i| i.raw.hovered_files.is_empty()) {
        return;
    }
    let rect = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop")));
    painter.rect_filled(rect, 0.0, Color32::from_black_alpha(170));
    painter.rect_stroke(rect.shrink(12.0), 14.0, Stroke::new(2.0, ACCENT_LIGHT), egui::StrokeKind::Inside);
    painter.text(rect.center(), Align2::CENTER_CENTER, text, bold(20.0), TEXT);
}

// -------------------------------------------------------------------- drawing helpers

/// Sends press/release for Ctrl/Shift/Alt whenever the modifier state changes, so remote combos
/// match the controller exactly and no modifier ever sticks.
fn sync_mods(viewer: &Viewer, sent: &mut egui::Modifiers, now: egui::Modifiers) {
    const VK_SHIFT: u16 = 0x10;
    const VK_CONTROL: u16 = 0x11;
    const VK_MENU: u16 = 0x12;
    for (want, have, vk) in
        [(now.ctrl, sent.ctrl, VK_CONTROL), (now.shift, sent.shift, VK_SHIFT), (now.alt, sent.alt, VK_MENU)]
    {
        if want != have {
            viewer.send(Input::KeyVk { vk, down: want });
        }
    }
    sent.ctrl = now.ctrl;
    sent.shift = now.shift;
    sent.alt = now.alt;
}

fn dot(ui: &mut egui::Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size + 2.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, color);
}

fn pill(ui: &mut egui::Ui, color: Color32, text: &str) {
    egui::Frame::NONE
        .fill(color.gamma_multiply(0.18))
        .corner_radius(20)
        .inner_margin(egui::Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                dot(ui, color, 8.0);
                ui.label(RichText::new(text).color(color).size(13.0));
            });
        });
}

fn accent_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    filled_button(ui, text, ACCENT)
}

fn danger_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    filled_button(ui, text, DANGER)
}

fn filled_button(ui: &mut egui::Ui, text: &str, fill: Color32) -> egui::Response {
    let button = egui::Button::new(RichText::new(text).font(bold(14.0)).color(Color32::WHITE))
        .fill(fill)
        .stroke(Stroke::NONE)
        .min_size(vec2(96.0, 34.0));
    let r = ui.add(button);
    if r.hovered() {
        ui.painter().rect_filled(r.rect, 8.0, Color32::from_white_alpha(18));
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn logo_texture(ctx: &egui::Context) -> TextureHandle {
    let icon = crate::icon();
    let img = ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
    ctx.load_texture("logo", img, TextureOptions::LINEAR)
}

fn monitor_icon(p: &egui::Painter, center: egui::Pos2, color: Color32) {
    let s = Stroke::new(1.6, color);
    let screen = Rect::from_center_size(center - vec2(0.0, 2.0), vec2(18.0, 12.0));
    p.rect_stroke(screen, 2.0, s, egui::StrokeKind::Middle);
    p.line_segment([pos2(center.x, screen.bottom()), pos2(center.x, screen.bottom() + 3.5)], s);
    p.line_segment([pos2(center.x - 5.0, screen.bottom() + 4.0), pos2(center.x + 5.0, screen.bottom() + 4.0)], s);
}

fn dialog_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(14)
        .inner_margin(egui::Margin::same(22))
        .shadow(egui::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(120) })
}

/// UV rect that crops an image of `img_aspect` to fill a box of `box_aspect` (CSS "cover").
fn cover_uv(img_aspect: f32, box_aspect: f32) -> Rect {
    if img_aspect > box_aspect {
        let w = box_aspect / img_aspect;
        Rect::from_min_max(pos2((1.0 - w) / 2.0, 0.0), pos2((1.0 + w) / 2.0, 1.0))
    } else {
        let h = img_aspect / box_aspect;
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, h))
    }
}

fn save_thumbnail(id: &str, fb: &[u8], [w, h]: [usize; 2]) {
    let tw = THUMB_W.min(w);
    let th = (h * tw / w).max(1);
    let mut out = Vec::with_capacity(tw * th * 4);
    for y in 0..th {
        let sy = y * h / th;
        for x in 0..tw {
            let sx = x * w / tw;
            let i = (sy * w + sx) * 4;
            out.extend_from_slice(&fb[i..i + 4]);
        }
    }
    let mut jpeg = Vec::new();
    if jpeg_encoder::Encoder::new(&mut jpeg, 80).encode(&out, tw as u16, th as u16, jpeg_encoder::ColorType::Rgba).is_ok() {
        let path = thumb_path(id);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, jpeg);
    }
}

fn latin_tweak() -> (f32, f32) {
    #[cfg(debug_assertions)]
    if let (Ok(s), Ok(y)) = (std::env::var("RD_FONT_SCALE"), std::env::var("RD_FONT_Y")) {
        return (s.parse().unwrap_or(1.0), y.parse().unwrap_or(0.0));
    }
    (0.92, -0.12)
}

fn setup_style(ctx: &egui::Context) {
    // Thai text uses Noto Sans Thai (embedded, so every machine looks the same). It only covers
    // Thai, so Latin letters and digits come from Segoe UI, then egui's bundled fonts.
    let mut fonts = egui::FontDefinitions::default();
    let mut add = |name: &str, data: Vec<u8>| {
        fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(data)));
    };
    add("noto-thai", include_bytes!("../assets/fonts/NotoSansThai-Regular.ttf").to_vec());
    add("noto-thai-sb", include_bytes!("../assets/fonts/NotoSansThai-SemiBold.ttf").to_vec());
    let segoe = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf").ok().map(|d| add("segoe", d)).is_some();
    let segoe_sb = std::fs::read(r"C:\Windows\Fonts\seguisb.ttf").ok().map(|d| add("segoe-sb", d)).is_some();
    // Segoe UI sits higher and reads larger than Noto Sans Thai at the same size; nudge it so
    // mixed Thai/English text shares one baseline and visual size.
    let (scale, y_off) = latin_tweak();
    for name in ["segoe", "segoe-sb"] {
        if let Some(data) = fonts.font_data.get_mut(name) {
            let mut d = (**data).clone();
            d.tweak.scale = scale;
            d.tweak.y_offset_factor = y_off;
            *data = Arc::new(d);
        }
    }
    let defaults = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();

    let mut regular: Vec<String> = vec!["noto-thai".into()];
    if segoe {
        regular.push("segoe".into());
    }
    regular.extend(defaults.iter().cloned());
    let mut strong: Vec<String> = vec!["noto-thai-sb".into()];
    if segoe_sb {
        strong.push("segoe-sb".into());
    }
    strong.extend(regular.iter().cloned());
    fonts.families.insert(FontFamily::Proportional, regular);
    fonts.families.insert(FontFamily::Name("bold".into()), strong);
    fonts.families.entry(FontFamily::Monospace).or_default().push("noto-thai".into());
    ctx.set_fonts(fonts);

    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = SURFACE;
    v.extreme_bg_color = Color32::from_rgb(18, 18, 21);
    v.faint_bg_color = SURFACE2;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT_LIGHT;
    let r = CornerRadius::same(8);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.weak_bg_fill = SURFACE2;
    v.widgets.inactive.bg_fill = SURFACE2;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(55, 55, 62);
    v.widgets.hovered.bg_fill = Color32::from_rgb(55, 55, 62);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(80, 80, 90));
    v.widgets.active.weak_bg_fill = Color32::from_rgb(64, 64, 72);
    v.widgets.active.bg_fill = Color32::from_rgb(64, 64, 72);
    v.text_edit_bg_color = Some(Color32::from_rgb(18, 18, 21));
    ctx.set_visuals(v);

    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(14.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        s.text_styles.insert(egui::TextStyle::Body, FontId::proportional(14.5));
        s.text_styles.insert(egui::TextStyle::Button, FontId::proportional(14.0));
        s.text_styles.insert(egui::TextStyle::Small, FontId::proportional(12.0));
    });
}
