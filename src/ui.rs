use crate::{
    model::{Cycle, Subscription},
    platform::{self, Platform},
    reminders,
    storage::{Settings, Store},
    updates,
};
use chrono::Local;
use eframe::egui::{self, Color32, RichText};
use std::sync::{Arc, Mutex, atomic::Ordering};

const BG: Color32 = Color32::from_rgb(17, 21, 32);
const PANEL: Color32 = Color32::from_rgb(24, 29, 43);
const CARD: Color32 = Color32::from_rgb(29, 35, 50);
const MUTED: Color32 = Color32::from_rgb(153, 166, 190);
const ACCENT: Color32 = Color32::from_rgb(126, 155, 255);
const WARNING: Color32 = Color32::from_rgb(248, 191, 107);

struct Editor {
    sub: Subscription,
    original: Option<Subscription>,
    date: String,
    error: String,
}
pub struct App {
    store: Arc<Mutex<Store>>,
    platform: Arc<Platform>,
    updates: updates::Shared,
    page: usize,
    filter: usize,
    search: String,
    editor: Option<Editor>,
    delete: Option<uuid::Uuid>,
    draft: Settings,
    token: String,
    autostart: bool,
    notice: String,
    task_status: Arc<Mutex<Option<String>>>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        store: Arc<Mutex<Store>>,
        platform: Arc<Platform>,
        hwnd: isize,
    ) -> Self {
        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = BG;
        style.visuals.window_fill = PANEL;
        style.visuals.override_text_color = Some(Color32::from_rgb(229, 235, 247));
        style.visuals.selection.bg_fill = Color32::from_rgb(54, 76, 139);
        style.visuals.widgets.inactive.bg_fill = CARD;
        style.visuals.widgets.inactive.weak_bg_fill = CARD;
        style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(48, 59, 83);
        style.visuals.widgets.active.bg_fill = Color32::from_rgb(59, 78, 129);
        style.spacing.item_spacing = egui::vec2(10., 10.);
        style.spacing.button_padding = egui::vec2(14., 9.);
        style.spacing.interact_size.y = 34.;
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(15.));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(15.));
        style
            .text_styles
            .insert(egui::TextStyle::Small, egui::FontId::proportional(12.));
        style
            .text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::proportional(27.));
        cc.egui_ctx.set_style(style);
        platform.bind(hwnd, cc.egui_ctx.clone());
        let draft = store.lock().unwrap().settings.clone();
        let token = platform::unprotect(&draft.telegram_token_protected).unwrap_or_default();
        let updates = Arc::new(Mutex::new(updates::State::Idle));
        if draft.startup_update_check {
            updates::check(updates.clone(), cc.egui_ctx.clone());
        }
        Self {
            store,
            platform,
            updates,
            page: 0,
            filter: 0,
            search: String::new(),
            editor: None,
            delete: None,
            draft,
            token,
            autostart: platform::autostart_enabled(),
            notice: String::new(),
            task_status: Arc::new(Mutex::new(None)),
        }
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("navigation")
            .exact_width(196.)
            .resizable(false)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(22))
            .show(ctx, |ui| {
                ui.add_space(12.);
                ui.label(
                    RichText::new("SR / REMINDER")
                        .color(ACCENT)
                        .strong()
                        .size(16.),
                );
                ui.add_space(26.);
                for (i, title) in ["Subscriptions", "Settings"].iter().enumerate() {
                    if ui
                        .add_sized(
                            [152., 42.],
                            egui::Button::new(*title).selected(self.page == i),
                        )
                        .clicked()
                    {
                        self.page = i;
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    if ui.button("Quit application").clicked() {
                        self.platform.request_quit();
                    }
                    ui.label(
                        RichText::new(format!("Version {}", updates::VERSION))
                            .small()
                            .color(MUTED),
                    );
                    ui.add_space(12.);
                    ui.label(
                        RichText::new("Closing the window keeps reminders running in the tray.")
                            .small()
                            .color(MUTED),
                    );
                });
            });
    }

    fn list(&mut self, ui: &mut egui::Ui) {
        let today = Local::now().date_naive();
        let mut subs = self.store.lock().unwrap().db.subscriptions.clone();
        subs.sort_by_key(|s| (!s.active, s.due, s.name.to_lowercase()));
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.heading("Your subscriptions");
                ui.label(RichText::new(today.format("%A, %d %B %Y").to_string()).color(MUTED));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(RichText::new("+ Add subscription").strong())
                            .fill(Color32::from_rgb(65, 89, 188)),
                    )
                    .clicked()
                {
                    let sub = Subscription::new(today);
                    self.editor = Some(Editor {
                        date: sub.due.to_string(),
                        sub,
                        original: None,
                        error: String::new(),
                    });
                }
            });
        });
        ui.add_space(22.);
        ui.columns(3, |columns| {
            let metrics = [
                ("ACTIVE", subs.iter().filter(|s| s.active).count(), ACCENT),
                (
                    "DUE WITHIN 7 DAYS",
                    subs.iter()
                        .filter(|s| s.active && (0..=7).contains(&s.days_left(today)))
                        .count(),
                    WARNING,
                ),
                (
                    "OVERDUE",
                    subs.iter()
                        .filter(|s| s.active && s.days_left(today) < 0)
                        .count(),
                    Color32::from_rgb(249, 133, 149),
                ),
            ];
            for (col, (label, count, color)) in columns.iter_mut().zip(metrics) {
                egui::Frame::new()
                    .fill(CARD)
                    .corner_radius(12)
                    .inner_margin(18)
                    .show(col, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(RichText::new(label).small().color(MUTED));
                        ui.label(
                            RichText::new(count.to_string())
                                .size(32.)
                                .color(color)
                                .strong(),
                        );
                    });
            }
        });
        ui.add_space(18.);
        ui.horizontal(|ui| {
            for (i, text) in ["All", "Needs attention", "Paused"].iter().enumerate() {
                if ui.selectable_label(self.filter == i, *text).clicked() {
                    self.filter = i;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search subscriptions...")
                        .desired_width(220.),
                );
            });
        });
        ui.add_space(12.);
        let query = self.search.trim().to_lowercase();
        let filtered: Vec<_> = subs
            .iter()
            .filter(|s| {
                (query.is_empty()
                    || format!("{} {}", s.name, s.notes)
                        .to_lowercase()
                        .contains(&query))
                    && match self.filter {
                        1 => s.needs_reminder(today),
                        2 => !s.active,
                        _ => true,
                    }
            })
            .collect();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if filtered.is_empty() {
                egui::Frame::new().fill(CARD).corner_radius(12).inner_margin(32).show(ui, |ui| {
                    ui.set_min_width(ui.available_width()); ui.label(RichText::new(if subs.is_empty() { "A little less to keep in mind." } else { "No matching subscriptions." }).size(22.));
                    ui.label(RichText::new("Add a subscription, choose its expiry date, and we will remind you before it ends.").color(MUTED));
                });
            }
            for sub in filtered {
                let days = sub.days_left(today);
                egui::Frame::new().fill(CARD).corner_radius(12).inner_margin(18).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&sub.name).size(20.).strong());
                            let price = if sub.amount.is_empty() { sub.cycle.label().to_string() } else { format!("{} {} / {}", sub.amount, sub.currency, sub.cycle.label().to_lowercase()) };
                            ui.label(RichText::new(price).color(MUTED));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let (text, color) = if !sub.active { ("Paused".into(), MUTED) } else if days < 0 { (format!("{} {} overdue", -days, if days == -1 { "day" } else { "days" }), Color32::from_rgb(249, 133, 149)) }
                                else if days == 0 { ("Due today".into(), WARNING) } else { (format!("In {days} {}", if days == 1 { "day" } else { "days" }), if days <= sub.reminder_days as i64 { WARNING } else { ACCENT }) };
                            ui.label(RichText::new(text).color(color).strong());
                        });
                    });
                    ui.add_space(5.);
                    ui.label(RichText::new(format!("Expires {}  /  Remind {} {} before", sub.due, sub.reminder_days, if sub.reminder_days == 1 { "day" } else { "days" })).small().color(MUTED));
                    if !sub.notes.is_empty() { ui.label(RichText::new(&sub.notes).color(MUTED)); }
                    ui.add_space(6.);
                    ui.horizontal(|ui| {
                        if ui.button("Edit").clicked() { self.editor = Some(Editor { sub: sub.clone(), original: Some(sub.clone()), date: sub.due.to_string(), error: String::new() }); }
                        if sub.cycle != Cycle::OneTime && ui.button("Mark renewed").on_hover_text("Move the expiry one billing period forward from the expiry date, or today if overdue.").clicked() {
                            let mut store = self.store.lock().unwrap(); let mut db = store.db.clone();
                            if let Some(item) = db.subscriptions.iter_mut().find(|s| s.id == sub.id) { item.renew(today); }
                            self.notice = match store.commit(db) { Ok(()) => "Expiry date renewed.".into(), Err(e) => e };
                        }
                        if ui.small_button(if sub.active { "Pause" } else { "Resume" }).clicked() {
                            let mut store = self.store.lock().unwrap(); let mut db = store.db.clone();
                            if let Some(item) = db.subscriptions.iter_mut().find(|s| s.id == sub.id) { item.active = !item.active; }
                            self.notice = match store.commit(db) { Ok(()) => "Subscription updated.".into(), Err(e) => e };
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| { if ui.small_button("Delete").clicked() { self.delete = Some(sub.id); } });
                    });
                });
                ui.add_space(4.);
            }
        });
    }

    fn settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading("Settings");
        ui.label(RichText::new("Make reminders work your way.").color(MUTED));
        ui.add_space(20.);
        egui::ScrollArea::vertical().show(ui, |ui| {
            section(ui, "Reminders", |ui| {
                ui.checkbox(&mut self.draft.windows_notifications, "Windows notifications");
                ui.label(RichText::new("Checked every 30 seconds. One reminder per day until renewed or paused.").small().color(MUTED));
                let before = self.autostart;
                if ui.checkbox(&mut self.autostart, "Start with Windows (in the system tray)").changed() {
                    let config = self.store.lock().unwrap().config_dir.clone();
                    if let Err(e) = platform::set_autostart(self.autostart, &config) { self.notice = e; self.autostart = before; }
                }
                if ui.button("Test Windows notification").clicked() {
                    let platform = self.platform.clone(); let status = self.task_status.clone(); let ctx = ctx.clone();
                    std::thread::spawn(move || { *status.lock().unwrap() = Some(match platform.notify("Subscriptions Reminder".into(), "Your Windows reminders are ready.".into()) { Ok(()) => "Test notification submitted to Windows. Check your notification settings if it is hidden.".into(), Err(e) => e }); ctx.request_repaint(); });
                }
            });
            section(ui, "Telegram · optional", |ui| {
                ui.checkbox(&mut self.draft.telegram_enabled, "Also send reminders to Telegram");
                ui.label(RichText::new("Create a bot with @BotFather, start a chat with it, then enter its token and your chat ID.").small().color(MUTED));
                ui.label("Bot token"); ui.add(egui::TextEdit::singleline(&mut self.token).password(true).desired_width(480.));
                ui.label("Chat ID"); ui.add(egui::TextEdit::singleline(&mut self.draft.telegram_chat_id).desired_width(240.));
                ui.label(RichText::new("The token is encrypted for this Windows account and stays outside the synced data folder. Enabled reminders send subscription names and dates to Telegram.").small().color(MUTED));
                if ui.button("Send test using saved settings").clicked() {
                    let settings = self.store.lock().unwrap().settings.clone(); let status = self.task_status.clone(); let ctx = ctx.clone();
                    std::thread::spawn(move || { *status.lock().unwrap() = Some(match reminders::send_telegram(&settings, "Subscriptions Reminder: your test reminder is working.") { Ok(()) => "Telegram test delivered.".into(), Err(e) => e }); ctx.request_repaint(); });
                }
            });
            section(ui, "Data & Google Drive", |ui| {
                let dir = self.store.lock().unwrap().settings.data_dir.clone();
                ui.label(RichText::new(dir.display().to_string()).monospace().small());
                ui.label(RichText::new("Choose a folder managed by Google Drive for desktop to sync subscriptions and notes. An existing subscriptions.json is loaded; an empty folder receives your current list.").small().color(MUTED));
                ui.label(RichText::new("Use one computer at a time and wait for Drive to finish syncing before switching. JSON backups are saved alongside the data.").small().color(MUTED));
                if ui.button("Choose data folder...").clicked()
                    && let Some(dir) = rfd::FileDialog::new().set_directory(&dir).pick_folder() {
                        let mut store = self.store.lock().unwrap();
                        self.notice = match store.use_folder(dir) { Ok(()) => { self.draft.data_dir = store.settings.data_dir.clone(); "Data folder updated.".into() }, Err(e) => e };
                }
            });
            section(ui, "Updates", |ui| {
                ui.label(format!("Installed version: {} · Windows x64", updates::VERSION));
                ui.checkbox(&mut self.draft.startup_update_check, "Check for updates on startup");
                ui.label(RichText::new("Checks GitHub without blocking launch. Download and installation only start when you click below.").small().color(MUTED));
                if ui.button("Check for updates").clicked() { updates::check(self.updates.clone(), ctx.clone()); }
                let state = self.updates.lock().unwrap().clone();
                match state {
                    updates::State::Idle => (),
                    updates::State::Checking => { ui.label("Checking GitHub..."); },
                    updates::State::Current => { ui.label("You are up to date."); },
                    updates::State::Error(e) => { ui.colored_label(WARNING, e); },
                    updates::State::Downloading(progress) => { ui.add(egui::ProgressBar::new(progress).show_percentage().text("Downloading and verifying update...")); },
                    updates::State::Available(release) => {
                        ui.colored_label(ACCENT, format!("Version {} is available.", release.tag_name));
                        if ui.button("Download and install").clicked() {
                            let config = self.store.lock().unwrap().config_dir.clone();
                            updates::install(release, self.updates.clone(), ctx.clone(), self.platform.clone(), config);
                        }
                    }
                }
            });
            if ui.add(egui::Button::new("Save settings").fill(Color32::from_rgb(65, 89, 188))).clicked() {
                let result = (|| -> Result<(), String> {
                    if self.draft.telegram_enabled && (self.token.trim().is_empty() || self.draft.telegram_chat_id.trim().is_empty()) { return Err("Enter a Telegram token and chat ID, or disable Telegram reminders.".into()); }
                    self.draft.telegram_token_protected = platform::protect(self.token.trim())?;
                    let mut store = self.store.lock().unwrap(); let previous = store.settings.clone();
                    let mut next = self.draft.clone(); next.deliveries = store.settings.deliveries.clone(); next.data_dir = store.settings.data_dir.clone();
                    store.settings = next;
                    if let Err(e) = store.save_settings() { store.settings = previous; return Err(e); }
                    Ok(())
                })();
                self.notice = result.err().unwrap_or_else(|| "Settings saved.".into());
            }
        });
    }

    fn editor(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.editor.take() else {
            return;
        };
        let mut open = true;
        let mut finished = false;
        egui::Window::new(if editor.original.is_some() { "Edit subscription" } else { "Add subscription" }).id(egui::Id::new("editor"))
            .open(&mut open).collapsible(false).resizable(false).default_width(450.).anchor(egui::Align2::CENTER_CENTER, [0., 0.]).show(ctx, |ui| {
                ui.label("Subscription name"); ui.add(egui::TextEdit::singleline(&mut editor.sub.name).desired_width(f32::INFINITY));
                ui.horizontal(|ui| {
                    ui.vertical(|ui| { ui.label("Expiry date (YYYY-MM-DD)"); ui.add(egui::TextEdit::singleline(&mut editor.date).desired_width(180.)); });
                    ui.vertical(|ui| { ui.label("Remind days before"); ui.add(egui::DragValue::new(&mut editor.sub.reminder_days).range(0..=365)); });
                });
                ui.label("Billing period");
                egui::ComboBox::from_id_salt("cycle").selected_text(editor.sub.cycle.label()).show_ui(ui, |ui| { for cycle in Cycle::ALL { ui.selectable_value(&mut editor.sub.cycle, cycle, cycle.label()); } });
                ui.horizontal(|ui| {
                    ui.vertical(|ui| { ui.label("Price (optional)"); ui.add(egui::TextEdit::singleline(&mut editor.sub.amount).desired_width(150.)); });
                    ui.vertical(|ui| { ui.label("Currency"); ui.add(egui::TextEdit::singleline(&mut editor.sub.currency).desired_width(80.)); });
                });
                ui.label("Notes"); ui.add(egui::TextEdit::multiline(&mut editor.sub.notes).desired_rows(3).desired_width(f32::INFINITY));
                ui.checkbox(&mut editor.sub.active, "Active reminders");
                if !editor.error.is_empty() { ui.colored_label(WARNING, &editor.error); }
                ui.horizontal(|ui| {
                    if ui.button("Save subscription").clicked() {
                        let result = (|| -> Result<(), String> {
                            editor.sub.due = chrono::NaiveDate::parse_from_str(editor.date.trim(), "%Y-%m-%d").map_err(|_| "Enter a valid expiry date as YYYY-MM-DD.".to_string())?;
                            editor.sub.name = editor.sub.name.trim().into(); editor.sub.amount = editor.sub.amount.trim().into();
                            editor.sub.validate()?;
                            let mut store = self.store.lock().unwrap(); store.refresh()?;
                            if let Some(original) = &editor.original
                                && store.db.subscriptions.iter().find(|s| s.id == original.id) != Some(original) { return Err("This subscription changed outside the editor. Close this editor and open it again.".into()); }
                            let mut db = store.db.clone();
                            if let Some(item) = db.subscriptions.iter_mut().find(|s| s.id == editor.sub.id) { *item = editor.sub.clone(); } else { db.subscriptions.push(editor.sub.clone()); }
                            store.commit(db)
                        })();
                        match result { Ok(()) => { finished = true; self.notice = "Subscription saved.".into(); }, Err(e) => editor.error = e }
                    }
                    if ui.button("Cancel").clicked() { finished = true; }
                });
            });
        if open && !finished {
            self.editor = Some(editor);
        }
    }
}

fn section(ui: &mut egui::Ui, title: &str, content: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(12)
        .inner_margin(18)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(title).size(18.).strong());
            ui.add_space(3.);
            content(ui);
        });
    ui.add_space(6.);
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested())
            && !self.platform.quit.load(Ordering::SeqCst)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        if let Some(status) = self.task_status.lock().unwrap().take() {
            self.notice = status;
        }
        let background_error = self.store.lock().unwrap().error.take();
        if let Some(error) = background_error {
            self.notice = error;
        }
        self.sidebar(ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(28))
            .show(ctx, |ui| {
                let available =
                    matches!(*self.updates.lock().unwrap(), updates::State::Available(_));
                if available && self.page != 1 {
                    ui.horizontal(|ui| {
                        ui.colored_label(ACCENT, "A new version is available.");
                        if ui.button("View update").clicked() {
                            self.page = 1;
                        }
                    });
                    ui.add_space(8.);
                }
                if !self.notice.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(WARNING, &self.notice);
                        if ui.small_button("Dismiss").clicked() {
                            self.notice.clear();
                        }
                    });
                    ui.add_space(8.);
                }
                if self.page == 0 {
                    self.list(ui);
                } else {
                    self.settings(ui, ctx);
                }
            });
        self.editor(ctx);
        if let Some(id) = self.delete {
            egui::Window::new("Delete subscription?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0., 0.])
                .show(ctx, |ui| {
                    ui.label(
                        "This removes the subscription and its notes from the current data file.",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            let mut store = self.store.lock().unwrap();
                            let mut db = store.db.clone();
                            db.subscriptions.retain(|s| s.id != id);
                            self.notice = match store.commit(db) {
                                Ok(()) => "Subscription deleted.".into(),
                                Err(e) => e,
                            };
                            self.delete = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.delete = None;
                        }
                    });
                });
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(15));
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.platform.stop();
    }
}
