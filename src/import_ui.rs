use crate::theme::Palette;
use crate::{ai, model::Cycle, platform, storage::Store};
use egui::{self, RichText};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    thread,
};

#[derive(Default)]
enum Task {
    #[default]
    Idle,
    Working,
    Done(ai::Recognition),
    Error(String),
}
#[derive(Default)]
enum Models {
    #[default]
    Idle,
    Working(String),
    Done(String, Vec<String>),
    Error(String, String),
}

pub enum Navigation {
    Stay,
    Subscriptions,
    Settings,
}

pub struct Importer {
    attachment: Option<ai::Attachment>,
    texture: Option<egui::TextureHandle>,
    task: Arc<Mutex<Task>>,
    models: Arc<Mutex<Models>>,
    keys: [String; 3],
    review: Option<ai::Recognition>,
    notice: String,
    source: String,
}
impl Importer {
    pub fn new(settings: &ai::Settings) -> Self {
        let keys = ai::Provider::ALL.map(|p| {
            platform::unprotect(&settings.connection(p).api_key_protected).unwrap_or_default()
        });
        Self {
            attachment: None,
            texture: None,
            task: Arc::new(Mutex::new(Task::Idle)),
            models: Arc::new(Mutex::new(Models::Idle)),
            keys,
            review: None,
            notice: String::new(),
            source: String::new(),
        }
    }
    pub fn busy(&self) -> bool {
        matches!(*self.task.lock().unwrap(), Task::Working)
    }
    pub fn load_file(&mut self, path: &Path, ctx: &egui::Context) -> bool {
        if self.busy() {
            self.notice = "Wait for the current recognition request to finish.".into();
            return false;
        }
        let image = ai::Attachment::file(path);
        self.attach(image, ctx)
    }
    fn attach(&mut self, result: Result<ai::Attachment, String>, ctx: &egui::Context) -> bool {
        match result {
            Ok(image) => {
                self.texture = Some(ctx.load_texture(
                    "subscription-image",
                    egui::ColorImage::from_rgba_unmultiplied(
                        [image.width as usize, image.height as usize],
                        &image.rgba,
                    ),
                    egui::TextureOptions::LINEAR,
                ));
                self.attachment = Some(image);
                self.review = None;
                self.notice.clear();
                true
            }
            Err(error) => {
                self.notice = error;
                false
            }
        }
    }
    pub fn paste(&mut self, store: &Arc<Mutex<Store>>, ctx: &egui::Context) {
        if !self.busy() && self.attach(ai::Attachment::clipboard(), ctx) {
            self.recognize(store, ctx);
        }
    }
    pub fn report(&self) -> Option<&ai::Recognition> {
        self.review.as_ref()
    }
    pub fn recognize(&mut self, shared: &Arc<Mutex<Store>>, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let Some(image) = self.attachment.clone() else {
            self.notice = "Choose or paste an image first.".into();
            return;
        };
        let settings = shared.lock().unwrap().settings.ai.clone();
        if !settings.enabled {
            self.notice =
                "Enable AI image import in Settings and save the connection first.".into();
            return;
        }
        if settings.active().model.trim().is_empty() {
            self.notice = "Choose a vision model in Settings and save it first.".into();
            return;
        }
        self.source = format!(
            "{} / {}",
            settings.provider.label(),
            settings.active().model
        );
        self.review = None;
        self.notice.clear();
        *self.task.lock().unwrap() = Task::Working;
        let task = self.task.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result =
                ai::recognize(settings.active(), &image, chrono::Local::now().date_naive());
            *task.lock().unwrap() = match result {
                Ok(value) => Task::Done(value),
                Err(error) => Task::Error(error),
            };
            ctx.request_repaint();
        });
    }
    pub fn poll(&mut self) {
        let mut task = self.task.lock().unwrap();
        if matches!(*task, Task::Done(_) | Task::Error(_)) {
            match std::mem::replace(&mut *task, Task::Idle) {
                Task::Done(value) => {
                    if value.candidates.is_empty() {
                        self.notice = "No subscriptions were found in this image.".into();
                    }
                    self.review = Some(value);
                }
                Task::Error(error) => self.notice = error,
                _ => (),
            }
        }
    }
    pub fn settings(&mut self, ui: &mut egui::Ui, draft: &mut ai::Settings, ctx: &egui::Context) {
        ui.checkbox(&mut draft.enabled, "Enable AI image import");
        egui::ComboBox::from_id_salt("ai-provider")
            .selected_text(draft.provider.label())
            .show_ui(ui, |ui| {
                for provider in ai::Provider::ALL {
                    ui.selectable_value(&mut draft.provider, provider, provider.label());
                }
            });
        let provider = draft.provider;
        let index = provider.index();
        let connection = draft.connection_mut(provider);
        ui.label("Base URL");
        if ui
            .add(egui::TextEdit::singleline(&mut connection.base_url).desired_width(530.))
            .changed()
        {
            if !self.keys[index].is_empty() {
                self.notice = "The API key was cleared because the endpoint changed. Enter the key for the new endpoint.".into();
            }
            self.keys[index].clear();
            connection.api_key_protected.clear();
        }
        ui.label("API key (optional for servers without authentication)");
        ui.add(
            egui::TextEdit::singleline(&mut self.keys[index])
                .password(true)
                .desired_width(530.),
        );
        ui.horizontal(|ui| {
            ui.label("Vision model ID");
            let pending = matches!(*self.models.lock().unwrap(), Models::Working(_));
            if ui
                .add_enabled(!pending, egui::Button::new("Load models"))
                .clicked()
            {
                match platform::protect(self.keys[index].trim()) {
                    Err(error) => self.notice = error,
                    Ok(key) => {
                        let mut config = connection.clone();
                        config.api_key_protected = key;
                        let endpoint = config.base_url.clone();
                        *self.models.lock().unwrap() = Models::Working(endpoint.clone());
                        let state = self.models.clone();
                        let ctx = ctx.clone();
                        thread::spawn(move || {
                            *state.lock().unwrap() = match ai::load_models(&config) {
                                Ok(models) => Models::Done(endpoint, models),
                                Err(e) => Models::Error(endpoint, e),
                            };
                            ctx.request_repaint();
                        });
                    }
                }
            }
        });
        ui.add(
            egui::TextEdit::singleline(&mut connection.model)
                .hint_text(if provider == ai::Provider::AiHub {
                    "universal"
                } else {
                    "Enter a vision-capable model ID"
                })
                .desired_width(530.),
        );
        match &*self.models.lock().unwrap() {
            Models::Working(url) if *url == connection.base_url => {
                ui.label("Loading models...");
            }
            Models::Error(url, error) if *url == connection.base_url => {
                ui.colored_label(Palette::for_ui(ui).danger, error);
            }
            Models::Done(url, models) if *url == connection.base_url => {
                egui::ComboBox::from_id_salt("available-ai-models")
                    .selected_text("Select from available models...")
                    .height(220.)
                    .show_ui(ui, |ui| {
                        for model in models {
                            ui.selectable_value(&mut connection.model, model.clone(), model);
                        }
                    });
            }
            _ => (),
        }
        if provider == ai::Provider::AiHub {
            ui.label(
                RichText::new(
                    "Universal follows the active AI-Hub model. Select a model with image input.",
                )
                .small(),
            );
        }
        if provider == ai::Provider::OpenRouter {
            ui.label(RichText::new("Use an OpenRouter API key and a model with image input. Provider usage may be billed to your account.").small());
        }
        ui.collapsing("Privacy & connection details", |ui| {
            ui.label("Each provider has its own encrypted API key. Changing the URL clears its key. Images are sent only when you request recognition.");
            ui.label("Save settings before recognizing an image. OpenAI-compatible endpoints use Chat Completions with image input.");
        });
        if !self.notice.is_empty() {
            ui.colored_label(Palette::for_ui(ui).warning, &self.notice);
        }
    }
    pub fn encrypt_keys(&self, settings: &mut ai::Settings) -> Result<(), String> {
        for provider in ai::Provider::ALL {
            let config = settings.connection_mut(provider);
            config.base_url = ai::normalize_url(&config.base_url)?;
            if self.keys[provider.index()].contains(['\r', '\n']) {
                return Err("An API key cannot contain a line break.".into());
            }
            config.api_key_protected = platform::protect(self.keys[provider.index()].trim())?;
            config.model = config.model.trim().into();
        }
        Ok(())
    }
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        store: &Arc<Mutex<Store>>,
    ) -> Navigation {
        self.poll();
        let colors = Palette::for_ui(ui);
        let mut save_requested = false;
        let mut navigation = Navigation::Stay;
        ui.horizontal(|ui| {
            if ui
                .button("←")
                .on_hover_text("Back to subscriptions")
                .clicked()
            {
                navigation = Navigation::Subscriptions;
            }
            ui.heading("Image import");
            if self
                .review
                .as_ref()
                .is_some_and(|review| !review.candidates.is_empty())
            {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(colors.primary_button("Save selected subscriptions"))
                        .clicked()
                    {
                        save_requested = true;
                    }
                });
            }
        });
        let active = store.lock().unwrap().settings.ai.clone();
        ui.label(
            RichText::new(format!(
                "{} · {}",
                active.provider.label(),
                if active.active().model.is_empty() {
                    "Choose a vision model"
                } else {
                    &active.active().model
                }
            ))
            .small()
            .color(colors.muted),
        );
        ui.add_space(10.0);
        let busy = self.busy();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!busy, egui::Button::new("Attach and recognize"))
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
                    .pick_file()
                && self.load_file(&path, ctx)
            {
                self.recognize(store, ctx);
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Paste image"))
                .clicked()
            {
                self.paste(store, ctx);
            }
            if ui
                .add_enabled(
                    !busy && self.attachment.is_some(),
                    egui::Button::new("Recognize again"),
                )
                .clicked()
            {
                self.recognize(store, ctx);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Connection settings").clicked() {
                    navigation = Navigation::Settings;
                }
            });
        });
        ui.label(
            RichText::new(
                "PNG, JPEG, WebP · drop an image or press Ctrl+V · sent to your selected provider",
            )
            .small()
            .color(colors.muted),
        );
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading the image...");
            });
        }
        if !self.notice.is_empty() {
            ui.colored_label(colors.warning, &self.notice);
        }
        ui.add_space(8.0);
        egui::ScrollArea::vertical().id_salt("image-review").auto_shrink([false, false]).show(ui, |ui| {
            if let (Some(image), Some(texture)) = (&self.attachment, &self.texture) {
                ui.collapsing(format!("Source image · {} · {} × {}", image.name, image.width, image.height), |ui| {
                    let scale = (ui.available_width().min(360.0) / image.width as f32).min(120.0 / image.height as f32).min(1.0);
                    ui.image((texture.id(), egui::vec2(image.width as f32 * scale, image.height as f32 * scale)));
                });
            }
            if let Some(review) = &mut self.review {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("Review {} detected subscriptions", review.candidates.len())).size(14.0).strong());
                ui.label(RichText::new("Correct uncertain fields, then save the selected records.").small().color(colors.muted));
                if !review.warnings.is_empty() {
                    ui.collapsing(format!("Model notes ({})", review.warnings.len()), |ui| {
                        for warning in &review.warnings { ui.colored_label(colors.warning, warning); }
                    });
                }
                ui.add_space(4.0);
                for (index, item) in review.candidates.iter_mut().enumerate() {
                    ui.push_id(index, |ui| {
                        egui::Frame::new().fill(colors.card).stroke(egui::Stroke::new(1.0, colors.card_border)).inner_margin(10).corner_radius(6).show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut item.selected, "").on_hover_text("Include this subscription");
                                let name_width = (ui.available_width() - 320.0).max(160.0);
                                ui.add(egui::TextEdit::singleline(&mut item.sub.name).hint_text("Subscription name").desired_width(name_width));
                                ui.label("Expires");
                                ui.add(egui::TextEdit::singleline(&mut item.date).hint_text("YYYY-MM-DD").desired_width(115.0));
                                egui::ComboBox::from_id_salt("import-cycle").width(102.0).selected_text(item.sub.cycle.label()).show_ui(ui, |ui| {
                                    for cycle in Cycle::ALL { ui.selectable_value(&mut item.sub.cycle, cycle, cycle.label()); }
                                });
                            });
                            ui.horizontal(|ui| {
                                ui.label("Price");
                                ui.add(egui::TextEdit::singleline(&mut item.sub.amount).desired_width(78.0));
                                ui.add(egui::TextEdit::singleline(&mut item.sub.currency).hint_text("Currency").desired_width(60.0));
                                ui.add_space(10.0);
                                ui.label("Remind");
                                ui.add(egui::DragValue::new(&mut item.sub.reminder_days).range(0..=365));
                                ui.label("days before expiry");
                            });
                            ui.collapsing("Notes", |ui| {
                                ui.add(egui::TextEdit::multiline(&mut item.sub.notes).desired_rows(2).desired_width(f32::INFINITY));
                            });
                        });
                    });
                }
            } else if self.attachment.is_none() {
                egui::Frame::new().fill(colors.card).stroke(egui::Stroke::new(1.0, colors.card_border)).inner_margin(24).corner_radius(7).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(RichText::new("Drop a screenshot here").size(15.0).strong());
                    ui.label(RichText::new("You can also attach a file or paste an image from the clipboard.").color(colors.muted));
                });
            }
        });
        if save_requested && let Some(review) = &self.review {
            let result = ai::save_candidates(&mut store.lock().unwrap(), &review.candidates);
            match result {
                Ok((added, skipped)) => {
                    self.notice = format!(
                        "Saved {added} subscription(s). Skipped {skipped} exact duplicate(s)."
                    );
                    self.review = None;
                }
                Err(error) => self.notice = error,
            }
        }
        navigation
    }
}
