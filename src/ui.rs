use crate::{
    capture::Capture,
    drive,
    import_ui::Importer,
    model::{Cycle, Subscription},
    platform::{self, Platform},
    reminders,
    storage::{Settings, Store},
    theme::{self, Palette},
    updates,
};
use chrono::Local;
use egui::{self, RichText};
use std::sync::{Arc, Mutex, atomic::Ordering};

pub struct Launch {
    pub capture_dir: Option<std::path::PathBuf>,
    pub diagnostic: Option<std::path::PathBuf>,
    pub image: Option<std::path::PathBuf>,
    pub recognize: bool,
}

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
    settings_tab: usize,
    search: String,
    editor: Option<Editor>,
    delete: Option<uuid::Uuid>,
    draft: Settings,
    token: String,
    autostart: bool,
    notice: String,
    task_status: Arc<Mutex<Option<String>>>,
    importer: Importer,
    capture: Option<Capture>,
    drive: drive::Controller,
    drive_panel: drive::ui::Panel,
}

impl App {
    pub fn new(
        ctx: &egui::Context,
        store: Arc<Mutex<Store>>,
        platform: Arc<Platform>,
        hwnd: isize,
        launch: Launch,
    ) -> Self {
        platform.bind(hwnd, ctx.clone());
        let draft = store.lock().unwrap().settings.clone();
        theme::install(ctx, draft.theme);
        let token = platform::unprotect(&draft.telegram_token_protected).unwrap_or_default();
        let updates = Arc::new(Mutex::new(updates::State::Idle));
        if draft.startup_update_check {
            updates::check(updates.clone(), ctx.clone());
        }
        let mut importer = Importer::new(&draft.ai);
        let drive = drive::Controller::new(store.clone(), platform.clone(), ctx.clone());
        let page = if let Some(path) = &launch.image {
            if importer.load_file(path, ctx) && launch.recognize {
                importer.recognize(&store, ctx);
            }
            1
        } else {
            0
        };
        Self {
            store,
            platform,
            updates,
            page,
            filter: 0,
            settings_tab: 0,
            search: String::new(),
            editor: None,
            delete: None,
            draft,
            token,
            autostart: platform::autostart_enabled(),
            notice: String::new(),
            task_status: Arc::new(Mutex::new(None)),
            importer,
            capture: launch.capture_dir.map(Capture::new),
            drive,
            drive_panel: drive::ui::Panel::default(),
        }
    }

    fn open_editor(&mut self, sub: Subscription, existing: bool) {
        self.editor = Some(Editor {
            date: sub.due.to_string(),
            original: existing.then(|| sub.clone()),
            sub,
            error: String::new(),
        });
    }

    fn renew(&mut self, id: uuid::Uuid, today: chrono::NaiveDate) {
        let mut store = self.store.lock().unwrap();
        let mut db = store.db.clone();
        if let Some(item) = db.subscriptions.iter_mut().find(|item| item.id == id) {
            item.renew(today);
        }
        self.notice = store
            .commit(db)
            .err()
            .unwrap_or_else(|| "Expiry date renewed.".into());
    }

    fn toggle_active(&mut self, id: uuid::Uuid) {
        let mut store = self.store.lock().unwrap();
        let mut db = store.db.clone();
        if let Some(item) = db.subscriptions.iter_mut().find(|item| item.id == id) {
            item.active = !item.active;
        }
        self.notice = store
            .commit(db)
            .err()
            .unwrap_or_else(|| "Subscription updated.".into());
    }

    fn list(&mut self, ui: &mut egui::Ui) {
        let colors = Palette::for_ui(ui);
        let today = Local::now().date_naive();
        let mut subs = self.store.lock().unwrap().db.subscriptions.clone();
        subs.sort_by_key(|sub| (!sub.active, sub.due, sub.name.to_lowercase()));
        ui.horizontal(|ui| {
            ui.heading("Subscriptions");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Settings").clicked() {
                    self.page = 2;
                }
                if ui
                    .add(colors.primary_button("+ Add subscription"))
                    .clicked()
                {
                    self.open_editor(Subscription::new(today), false);
                }
                if ui.button("Import image").clicked() {
                    self.page = 1;
                }
            });
        });
        ui.label(
            RichText::new(today.format("%A, %d %B %Y").to_string())
                .small()
                .color(colors.muted),
        );
        ui.add_space(9.0);
        egui::Frame::new()
            .fill(colors.card)
            .corner_radius(6)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    for (count, label, color) in [
                        (
                            subs.iter().filter(|sub| sub.active).count(),
                            "active",
                            colors.text,
                        ),
                        (
                            subs.iter()
                                .filter(|sub| sub.active && (0..=7).contains(&sub.days_left(today)))
                                .count(),
                            "due within 7 days",
                            colors.warning,
                        ),
                        (
                            subs.iter()
                                .filter(|sub| sub.active && sub.days_left(today) < 0)
                                .count(),
                            "overdue",
                            colors.danger,
                        ),
                    ] {
                        ui.label(RichText::new(count.to_string()).strong().color(color));
                        ui.label(RichText::new(label).small().color(colors.muted));
                        ui.add_space(14.0);
                    }
                });
            });
        ui.add_space(7.0);
        ui.horizontal(|ui| {
            for (filter, title) in ["All", "Needs attention", "Paused"].iter().enumerate() {
                ui.selectable_value(&mut self.filter, filter, *title);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search name or notes")
                        .desired_width(210.0),
                );
            });
        });
        ui.add_space(7.0);
        let query = self.search.trim().to_lowercase();
        let filtered: Vec<_> = subs
            .iter()
            .filter(|sub| {
                (query.is_empty()
                    || sub.name.to_lowercase().contains(&query)
                    || sub.notes.to_lowercase().contains(&query))
                    && match self.filter {
                        1 => sub.needs_reminder(today),
                        2 => !sub.active,
                        _ => true,
                    }
            })
            .collect();
        let widths = row_widths(ui.available_width() - 12.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (width, title) in
                widths
                    .into_iter()
                    .zip(["SUBSCRIPTION", "PRICE / PERIOD", "EXPIRES", "STATUS", ""])
            {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 24.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_min_width(width);
                        ui.add_space(10.0);
                        ui.label(RichText::new(title).size(10.0).color(colors.muted));
                    },
                );
            }
        });
        ui.separator();
        if filtered.is_empty() {
            ui.add_space(28.0);
            ui.label(
                RichText::new(if subs.is_empty() {
                    "No subscriptions yet"
                } else {
                    "No matching subscriptions"
                })
                .size(15.0)
                .strong(),
            );
            ui.label(
                RichText::new(if subs.is_empty() {
                    "Add a subscription or import a screenshot to get started."
                } else {
                    "Try another search or filter."
                })
                .color(colors.muted),
            );
            return;
        }
        let size = self.draft.subscription_size;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::vertical()
                .id_salt(("subscription-table", size.label()))
                .auto_shrink([false, false])
                .max_height((ui.available_height() - 24.0).max(0.0))
                .show_rows(ui, size.height(), filtered.len(), |ui, range| {
                    for index in range {
                        let sub = filtered[index];
                        ui.push_id(sub.id, |ui| {
                            self.subscription_row(ui, sub, today, widths, index)
                        });
                    }
                });
        });
        ui.add_space(5.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} of {} subscriptions · ordered by expiry",
                    filtered.len(),
                    subs.len()
                ))
                .small()
                .color(colors.muted),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let status = self.drive.status();
                let (label, color) = if !status.conflicts.is_empty() {
                    ("Sync conflict · open Settings", colors.warning)
                } else {
                    match status.phase {
                        drive::Phase::Connected => ("Drive connected", colors.muted),
                        drive::Phase::Syncing => ("Synchronizing", colors.accent),
                        drive::Phase::Offline => ("Drive offline", colors.warning),
                        drive::Phase::SignInRequired => ("Drive needs sign-in", colors.warning),
                        _ => ("", colors.muted),
                    }
                };
                if !label.is_empty() {
                    ui.label(RichText::new(label).small().color(color))
                        .on_hover_text("Settings → Google Drive");
                }
            });
        });
    }

    fn subscription_row(
        &mut self,
        ui: &mut egui::Ui,
        sub: &Subscription,
        today: chrono::NaiveDate,
        widths: [f32; 5],
        index: usize,
    ) {
        let colors = Palette::for_ui(ui);
        let size = self.draft.subscription_size;
        let background = ui.painter().add(egui::Shape::Noop);
        let row = ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), size.height()),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                cell(ui, widths[0], size, |ui| {
                    if sub.notes.is_empty() {
                        ui.add_space(7.0);
                    }
                    ui.add(egui::Label::new(RichText::new(&sub.name).strong()).truncate())
                        .on_hover_text(&sub.name);
                    if !sub.notes.is_empty() {
                        ui.add(
                            egui::Label::new(RichText::new(&sub.notes).small().color(colors.muted))
                                .truncate(),
                        )
                        .on_hover_text(&sub.notes);
                    }
                });
                cell(ui, widths[1], size, |ui| {
                    let price = if sub.amount.is_empty() {
                        "—".into()
                    } else {
                        format!("{} {}", sub.amount, sub.currency)
                    };
                    ui.add(egui::Label::new(price).truncate());
                    ui.label(RichText::new(sub.cycle.label()).small().color(colors.muted));
                });
                cell(ui, widths[2], size, |ui| {
                    ui.label(sub.due.to_string());
                    ui.label(
                        RichText::new(format!("Remind {}d before", sub.reminder_days))
                            .small()
                            .color(colors.muted),
                    );
                });
                cell(ui, widths[3], size, |ui| {
                    let days = sub.days_left(today);
                    let (label, color) = if !sub.active {
                        ("Paused".into(), colors.muted)
                    } else if days < 0 {
                        (format!("{}d overdue", -days), colors.danger)
                    } else if days == 0 {
                        ("Due today".into(), colors.warning)
                    } else if days <= sub.reminder_days as i64 {
                        (format!("Due in {days}d"), colors.warning)
                    } else {
                        (format!("In {days} days"), colors.muted)
                    };
                    ui.add_space(7.0);
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_unmultiplied(
                            color.r(),
                            color.g(),
                            color.b(),
                            20,
                        ))
                        .inner_margin(egui::Margin::symmetric(6, 3))
                        .corner_radius(4)
                        .show(ui, |ui| {
                            ui.label(RichText::new(label).size(11.0).color(color));
                        });
                });
                ui.allocate_ui_with_layout(
                    egui::vec2(widths[4], size.height()),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if ui
                            .add_enabled(sub.cycle != Cycle::OneTime, egui::Button::new("Renew"))
                            .on_hover_text("Advance the expiry by one billing period.")
                            .clicked()
                        {
                            self.renew(sub.id, today);
                        }
                        if ui.button("Edit").clicked() {
                            self.open_editor(sub.clone(), true);
                        }
                        ui.menu_button("···", |ui| {
                            if ui
                                .button(if sub.active {
                                    "Pause reminders"
                                } else {
                                    "Resume reminders"
                                })
                                .clicked()
                            {
                                self.toggle_active(sub.id);
                                ui.close();
                            }
                            ui.separator();
                            if ui
                                .button(RichText::new("Delete subscription").color(colors.danger))
                                .clicked()
                            {
                                self.delete = Some(sub.id);
                                ui.close();
                            }
                        });
                    },
                );
            },
        );
        let hovered = row
            .response
            .rect
            .contains(ui.input(|input| input.pointer.hover_pos().unwrap_or(egui::Pos2::ZERO)));
        let fill = if hovered {
            colors.hovered
        } else if index.is_multiple_of(2) {
            colors.card
        } else {
            colors.background
        };
        ui.painter().set(
            background,
            egui::Shape::rect_filled(row.response.rect, 0, fill),
        );
        ui.painter().hline(
            row.response.rect.x_range(),
            row.response.rect.bottom(),
            egui::Stroke::new(1.0, colors.card_border),
        );
    }

    fn settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let colors = Palette::for_ui(ui);
        ui.horizontal(|ui| {
            if ui
                .button("←")
                .on_hover_text("Back to subscriptions")
                .clicked()
            {
                self.page = 0;
            }
            ui.heading("Settings");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(colors.primary_button("Save settings")).clicked() {
                    self.save_settings();
                }
            });
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            for (index, label) in [
                "General",
                "AI provider",
                "Notifications",
                "Google Drive",
                "Storage & updates",
            ]
            .iter()
            .enumerate()
            {
                ui.selectable_value(&mut self.settings_tab, index, *label);
            }
        });
        ui.add_space(10.0);
        if self.settings_tab == 3 {
            self.drive_panel.show(ui, &self.drive, &self.store);
            return;
        }
        egui::ScrollArea::vertical().id_salt(("settings", self.settings_tab)).show(ui, |ui| {
            match self.settings_tab {
                0 => {
                    section(ui, "Appearance", |ui| {
                        let previous_theme = self.draft.theme;
                        let previous_size = self.draft.subscription_size;
                        egui::Grid::new("appearance").num_columns(2).spacing([18.0, 8.0]).show(ui, |ui| {
                            ui.label("Theme");
                            ui.horizontal(|ui| {
                                for preference in theme::Preference::ALL {
                                    ui.selectable_value(&mut self.draft.theme, preference, preference.label());
                                }
                            }); ui.end_row();
                            ui.label("Subscription size");
                            ui.horizontal(|ui| {
                                for size in theme::SubscriptionSize::ALL {
                                    ui.selectable_value(&mut self.draft.subscription_size, size, size.label());
                                }
                            }); ui.end_row();
                        });
                        ui.label(RichText::new("Size controls row spacing. These appearance choices are saved immediately.").small().color(colors.muted));
                        if self.draft.theme != previous_theme || self.draft.subscription_size != previous_size {
                            let mut store = self.store.lock().unwrap();
                            let saved_theme = store.settings.theme;
                            let saved_size = store.settings.subscription_size;
                            store.settings.theme = self.draft.theme;
                            store.settings.subscription_size = self.draft.subscription_size;
                            if let Err(error) = store.save_settings() {
                                store.settings.theme = saved_theme;
                                store.settings.subscription_size = saved_size;
                                self.draft.theme = previous_theme;
                                self.draft.subscription_size = previous_size;
                                self.notice = error;
                            } else {
                                drop(store);
                                if self.draft.theme != previous_theme { self.draft.theme.apply(ctx); }
                                else { ctx.request_repaint(); }
                            }
                        }
                    });
                    section(ui, "Startup", |ui| {
                        let previous = self.autostart;
                        if ui.checkbox(&mut self.autostart, "Start with Windows").changed() {
                            let config = self.store.lock().unwrap().config_dir.clone();
                            if let Err(error) = platform::set_autostart(self.autostart, &config) {
                                self.notice = error; self.autostart = previous;
                            }
                        }
                        ui.label(RichText::new("Starts in the system tray with reminders running.").small().color(colors.muted));
                        ui.checkbox(&mut self.draft.startup_update_check, "Check for updates on startup");
                        ui.label(RichText::new("Checks in the background. Downloads and installation require your action.").small().color(colors.muted));
                    });
                    ui.add_space(8.0);
                    if ui.button("Quit application").clicked() { self.platform.request_quit(); }
                }
                1 => section(ui, "AI connection", |ui| self.importer.settings(ui, &mut self.draft.ai, ctx)),
                2 => {
                    section(ui, "Windows reminders", |ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.draft.windows_notifications, "Windows notifications");
                            if ui.button("Send test").clicked() {
                                let platform = self.platform.clone(); let status = self.task_status.clone(); let ctx = ctx.clone();
                                std::thread::spawn(move || {
                                    *status.lock().unwrap() = Some(match platform.notify("Subscriptions Reminder".into(), "Your Windows reminders are ready.".into()) {
                                        Ok(()) => "Test notification submitted to Windows.".into(), Err(error) => error,
                                    }); ctx.request_repaint();
                                });
                            }
                        });
                        ui.label(RichText::new("Checked every 30 seconds. Reminders repeat daily until renewed or paused.").small().color(colors.muted));
                    });
                    section(ui, "Telegram", |ui| {
                        ui.checkbox(&mut self.draft.telegram_enabled, "Also send reminders to Telegram");
                        egui::Grid::new("telegram-settings").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
                            ui.label("Bot token"); ui.add(egui::TextEdit::singleline(&mut self.token).password(true).desired_width(360.0)); ui.end_row();
                            ui.label("Chat ID"); ui.add(egui::TextEdit::singleline(&mut self.draft.telegram_chat_id).desired_width(200.0)); ui.end_row();
                        });
                        ui.collapsing("Setup help", |ui| {
                            ui.label("Create a bot with @BotFather, send /start in its chat, and enter the bot token and your chat ID. Save settings before testing.");
                            ui.label("Credentials stay encrypted on this computer. Reminders send subscription names and dates to Telegram.");
                        });
                        if ui.button("Test saved Telegram connection").clicked() {
                            let settings = self.store.lock().unwrap().settings.clone(); let status = self.task_status.clone(); let ctx = ctx.clone();
                            std::thread::spawn(move || {
                                *status.lock().unwrap() = Some(match reminders::send_telegram(&settings, "Subscriptions Reminder: your test reminder is working.") {
                                    Ok(()) => "Telegram test delivered.".into(), Err(error) => error,
                                }); ctx.request_repaint();
                            });
                        }
                    });
                }
                _ => {
                    section(ui, "Data & synchronization", |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Google Drive");
                            if ui.button("Manage connection").clicked() { self.settings_tab = 3; }
                        });
                        let store = self.store.lock().unwrap();
                        let dir = store.settings.data_dir.clone();
                        let connected = store.settings.drive.connected();
                        drop(store);
                        ui.label(RichText::new("Local offline copy").small().color(colors.muted));
                        ui.add(egui::Label::new(RichText::new(dir.display().to_string()).monospace().small()).truncate()).on_hover_text(dir.display().to_string());
                        if ui.add_enabled(!connected, egui::Button::new("Change local data folder")).on_hover_text("Disconnect Google Drive before changing the local data folder.").clicked()
                            && let Some(dir) = rfd::FileDialog::new().set_directory(&dir).pick_folder() {
                                let mut store = self.store.lock().unwrap();
                                self.notice = match store.use_folder(dir) {
                                    Ok(()) => { self.draft.data_dir = store.settings.data_dir.clone(); "Data folder updated.".into() }, Err(error) => error,
                                };
                            }
                    });
                    section(ui, "Updates", |ui| {
                        ui.horizontal(|ui| {
                            ui.label(format!("Version {} · Windows x64", updates::VERSION));
                            if ui.button("Check for updates").clicked() { updates::check(self.updates.clone(), ctx.clone()); }
                        });
                        let update_state = self.updates.lock().unwrap().clone();
                        match update_state {
                            updates::State::Idle => (),
                            updates::State::Checking => { ui.label("Checking GitHub..."); },
                            updates::State::Current => { ui.label("You are up to date."); },
                            updates::State::Error(error) => { ui.colored_label(colors.warning, error); },
                            updates::State::Downloading(progress) => { ui.add(egui::ProgressBar::new(progress).show_percentage().text("Downloading and verifying update...")); },
                            updates::State::Available(release) => {
                                ui.colored_label(colors.accent, format!("Version {} is available.", release.version));
                                if ui.add(colors.primary_button("Download and install")).clicked() {
                                    let config = self.store.lock().unwrap().config_dir.clone();
                                    updates::install(release, self.updates.clone(), ctx.clone(), self.platform.clone(), config);
                                }
                            }
                        }
                    });
                }
            }
        });
    }

    fn save_settings(&mut self) {
        let result = (|| -> Result<(), String> {
            self.importer.encrypt_keys(&mut self.draft.ai)?;
            if self.draft.telegram_enabled
                && (self.token.trim().is_empty() || self.draft.telegram_chat_id.trim().is_empty())
            {
                return Err(
                    "Enter a Telegram token and chat ID, or disable Telegram reminders.".into(),
                );
            }
            self.draft.telegram_token_protected = platform::protect(self.token.trim())?;
            let mut store = self.store.lock().unwrap();
            let previous = store.settings.clone();
            let mut next = self.draft.clone();
            next.deliveries = store.settings.deliveries.clone();
            next.drive = store.settings.drive.clone();
            next.data_dir = store.settings.data_dir.clone();
            store.settings = next;
            if let Err(e) = store.save_settings() {
                store.settings = previous;
                return Err(e);
            }
            Ok(())
        })();
        self.notice = result.err().unwrap_or_else(|| "Settings saved.".into());
    }

    fn editor(&mut self, ctx: &egui::Context) {
        let colors = Palette::for_context(ctx);
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
                if !editor.error.is_empty() { ui.colored_label(colors.warning, &editor.error); }
                ui.horizontal(|ui| {
                    if ui.add(colors.primary_button("Save subscription")).clicked() {
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

fn row_widths(width: f32) -> [f32; 5] {
    [(width - 468.0).max(150.0), 108.0, 112.0, 102.0, 146.0]
}

fn cell(
    ui: &mut egui::Ui,
    width: f32,
    size: theme::SubscriptionSize,
    contents: impl FnOnce(&mut egui::Ui),
) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, size.height()),
        egui::Layout::top_down(egui::Align::LEFT),
        |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 2.0;
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(10, size.padding()))
                .show(ui, |ui| {
                    ui.set_width((width - 20.0).max(1.0));
                    contents(ui);
                });
        },
    );
}

fn section(ui: &mut egui::Ui, title: &str, content: impl FnOnce(&mut egui::Ui)) {
    let colors = Palette::for_ui(ui);
    egui::Frame::new()
        .fill(colors.card)
        .stroke(egui::Stroke::new(1.0, colors.card_border))
        .corner_radius(7)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(title).size(13.).strong());
            ui.add_space(3.);
            content(ui);
        });
    ui.add_space(6.);
}

impl App {
    pub fn theme_preference(&self) -> theme::Preference {
        self.draft.theme
    }

    pub fn draw(&mut self, ctx: &egui::Context) {
        let colors = Palette::for_context(ctx);
        self.importer.poll();
        if let Some(capture) = &mut self.capture {
            if let Err(error) = capture.tick(ctx, self.importer.busy(), self.importer.report()) {
                self.notice = error;
                self.platform.request_quit();
            }
            self.page = capture.page();
            if let Some(tab) = capture.settings_tab() {
                self.settings_tab = tab;
            }
            if capture.done {
                self.platform.request_quit();
            }
        }
        if self.capture.as_ref().is_some_and(Capture::editor) && self.editor.is_none() {
            let sample = self.store.lock().unwrap().db.subscriptions.first().cloned();
            if let Some(sample) = sample {
                self.open_editor(sample, true);
            }
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
        if let Some(path) = dropped {
            self.page = 1;
            if self.importer.load_file(&path, ctx) {
                self.importer.recognize(&self.store, ctx);
            }
        }
        if self.page == 1
            && !ctx.wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::V))
        {
            self.importer.paste(&self.store, ctx);
        }
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
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(colors.background).inner_margin(18))
            .show(ctx, |ui| {
                let available =
                    matches!(*self.updates.lock().unwrap(), updates::State::Available(_));
                if available && self.page != 2 {
                    ui.horizontal(|ui| {
                        ui.colored_label(colors.accent, "A new version is available.");
                        if ui.button("View update").clicked() {
                            self.page = 2;
                            self.settings_tab = 4;
                        }
                    });
                    ui.add_space(8.);
                }
                if !self.notice.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(colors.warning, &self.notice);
                        if ui.small_button("Dismiss").clicked() {
                            self.notice.clear();
                        }
                    });
                    ui.add_space(8.);
                }
                match self.page {
                    0 => self.list(ui),
                    1 => match self.importer.show(ui, ctx, &self.store) {
                        crate::import_ui::Navigation::Stay => (),
                        crate::import_ui::Navigation::Subscriptions => self.page = 0,
                        crate::import_ui::Navigation::Settings => {
                            self.page = 2;
                            self.settings_tab = 1;
                        }
                    },
                    _ => self.settings(ui, ctx),
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
        // Expiry labels change at local midnight. Background jobs wake the UI
        // explicitly when their data changes; idle windows need no heartbeat.
        let now = Local::now();
        let until_midnight = now
            .date_naive()
            .succ_opt()
            .and_then(|date| date.and_hms_opt(0, 0, 0))
            .and_then(|midnight| midnight.and_local_timezone(Local).earliest())
            .and_then(|midnight| (midnight - now).to_std().ok())
            .unwrap_or(std::time::Duration::from_secs(3600));
        ctx.request_repaint_after(until_midnight);
    }
    pub fn shutdown(&mut self) {
        self.platform.stop();
    }
}
