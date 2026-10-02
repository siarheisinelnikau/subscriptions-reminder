use super::{Controller, Phase, oauth, replica::Resolution};
use crate::storage::Store;
use crate::theme::Palette;
use egui::{self, RichText};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct Panel {
    notice: String,
}
impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, drive: &Controller, shared: &Arc<Mutex<Store>>) {
        ui.label(
            RichText::new("Automatic synchronization with a local offline copy.")
                .small()
                .color(Palette::for_ui(ui).muted),
        );
        ui.add_space(10.);
        let settings = shared.lock().unwrap().settings.drive.clone();
        let status = drive.status();
        let signing_in = status.phase == Phase::Authorizing;
        let syncing = status.phase == Phase::Syncing;
        let bundled = oauth::Client::bundled().ok().flatten();
        egui::ScrollArea::vertical().id_salt("drive-panel").show(ui, |ui| {
            frame(ui, "Connection", |ui| {
                if settings.connected() { ui.label(RichText::new(&settings.account_label).strong()); }
                ui.colored_label(if matches!(status.phase, Phase::Offline | Phase::SignInRequired) { Palette::for_ui(ui).warning } else { Palette::for_ui(ui).accent }, &status.message);
                if let Some(last) = status.last_success { ui.label(format!("Last synchronized: {}", last.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S"))); }
                if status.pending { ui.label("Local changes are waiting to be uploaded."); }
                if syncing { ui.spinner(); }
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(!signing_in && !syncing && settings.can_connect(), egui::Button::new(if settings.connected() { "Reconnect / change account" } else { "Connect Google Drive" })).clicked() { drive.connect(); }
                    if ui.add_enabled(settings.connected() && !signing_in && !syncing && !settings.needs_sign_in, egui::Button::new("Sync now")).clicked() { drive.sync_now(); }
                    if ui.add_enabled(settings.connected() && !signing_in, egui::Button::new("Disconnect this computer")).clicked() { drive.disconnect(); }
                });
                if signing_in {
                    ui.horizontal(|ui| {
                        if let Some(url) = &status.browser_url && ui.button("Open browser again").clicked() { let _ = webbrowser::open(url); }
                        if ui.button("Cancel sign-in").clicked() { drive.cancel_sign_in(); }
                    });
                }
                if !settings.connected() && !signing_in {
                    if settings.can_connect() {
                        ui.label("Choose your Google account in the browser and allow subscription sync. No setup files are needed.");
                    } else {
                        ui.colored_label(Palette::for_ui(ui).warning, "Google sign-in is not configured in this build. A release with the app's Google connection is required.");
                    }
                }
                let mut automatic = settings.auto_sync;
                if ui.add_enabled(!signing_in, egui::Checkbox::new(&mut automatic, "Synchronize automatically")).changed() {
                    let mut store = shared.lock().unwrap(); let previous = store.settings.drive.auto_sync; store.settings.drive.auto_sync = automatic;
                    if let Err(error) = store.save_settings() { store.settings.drive.auto_sync = previous; self.notice = error; }
                    else if automatic { drive.sync_now(); } else { drive.pause(); }
                }
                ui.label(RichText::new("Automatic sync runs after local changes and checks other computers every 60 seconds. Manual Sync now remains available when automatic sync is off.").small());
            });
            ui.collapsing("Advanced: custom Google connection", |ui| {
                frame(ui, "OAuth client override", |ui| {
                    ui.label("For developers and self-hosted builds. A custom client uses its own app-data space; use the same client on all your computers.");
                    ui.hyperlink_to("Custom client setup guide", "https://github.com/siarheisinelnikau/subscriptions-reminder/blob/main/docs/google-drive-setup.md");
                    if !settings.client_id.is_empty() { ui.label(RichText::new(format!("Client: {}", settings.client_id)).small().monospace()); }
                    if bundled.as_ref().is_some_and(|client| !settings.client_id.is_empty() && settings.client_id != client.id)
                        && ui.add_enabled(!settings.connected() && !signing_in && !syncing, egui::Button::new("Use built-in Google connection")).clicked() {
                            self.notice = drive.use_bundled_client().err().unwrap_or_else(|| "Built-in connection selected. Connect Google Drive to sign in.".into());
                        }
                    if ui.add_enabled(!settings.connected() && !signing_in && !syncing, egui::Button::new("Import Google OAuth JSON...")).clicked()
                        && let Some(path) = rfd::FileDialog::new().add_filter("Google OAuth client", &["json"]).pick_file() {
                            let result = (|| -> Result<(), String> {
                                if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 64 * 1024 { return Err("The OAuth configuration file is too large.".into()); }
                                drive.import(&std::fs::read(path).map_err(|e| e.to_string())?)
                            })();
                            self.notice = result.err().unwrap_or_else(|| "OAuth configuration submitted. Connect when the import finishes.".into());
                        }
                });
            });
            if !self.notice.is_empty() { ui.colored_label(Palette::for_ui(ui).warning, &self.notice); }
            ui.collapsing("What is synchronized & privacy", |ui| {
                ui.label("Subscription names, expiry dates, prices, notes, reminder lead times, and paused status.");
                ui.label(RichText::new("Data is kept in this application's private Google Drive app-data space. No Google Drive desktop folder or additional sync client is required. The app does not request access to all your Drive files.").small());
                ui.label(RichText::new("Google tokens, AI keys, Telegram credentials, and notification history stay on this computer. Cloud data uses Google's storage protection and HTTPS; this version does not add a master-password encryption layer.").small());
                ui.label(RichText::new("Connecting merges the current local subscription list with the selected Google account. Disconnecting retains both your local data and cloud snapshots.").small());
            });
            if !status.conflicts.is_empty() {
                ui.add_space(12.); ui.heading(format!("Review {} conflict(s)", status.conflicts.len()));
                ui.label("Independent edits are merged automatically. Choose a version when the same subscription changed concurrently; other versions remain safe until you resolve them.");
                let current = shared.lock().unwrap().db.clone();
                for conflict in status.conflicts {
                    ui.push_id(conflict.id, |ui| {
                        frame(ui, "Subscription conflict", |ui| {
                            for (index, choice) in conflict.choices.iter().enumerate() {
                                let is_local = current.subscriptions.iter().find(|s| s.id == conflict.id) == choice.as_ref();
                                ui.label(RichText::new(format!("Version {}{}", index + 1, if is_local { " · current local version" } else { "" })).strong());
                                if let Some(sub) = choice {
                                    ui.label(format!("{} · expires {} · {} {}", sub.name, sub.due, sub.amount, sub.currency));
                                    ui.label(format!("{} · reminder {} days before · {}", sub.cycle.label(), sub.reminder_days, if sub.active { "active" } else { "paused" }));
                                    if !sub.notes.is_empty() { ui.label(&sub.notes); }
                                } else { ui.label("This version deletes the subscription."); }
                                if ui.add_enabled(!syncing && !signing_in, egui::Button::new("Use this version and sync")).clicked() { drive.resolve(conflict.clone(), Resolution::Choose(index)); }
                                ui.separator();
                            }
                            if conflict.choices.iter().filter(|s| s.is_some()).count() > 1 && ui.add_enabled(!syncing && !signing_in, egui::Button::new("Keep all subscription versions and sync")).clicked() { drive.resolve(conflict.clone(), Resolution::KeepAll); }
                        });
                    });
                }
            }
        });
    }
}
fn frame(ui: &mut egui::Ui, title: &str, content: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(Palette::for_ui(ui).card)
        .stroke(egui::Stroke::new(1.0, Palette::for_ui(ui).card_border))
        .corner_radius(7)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(title).size(13.).strong());
            ui.add_space(4.);
            content(ui);
        });
    ui.add_space(10.);
}
