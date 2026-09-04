use crate::{ai, model::Cycle, platform, storage::Store};
use egui::{self, Color32, RichText};
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
                ui.colored_label(Color32::LIGHT_RED, error);
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
            ui.label(RichText::new("Use your AI-Hub /v1 URL and access token. Universal routes to the active local model; that model must support images.").small());
        }
        if provider == ai::Provider::OpenRouter {
            ui.label(RichText::new("Use an OpenRouter API key and a model with image input. Provider usage may be billed to your account.").small());
        }
        ui.label(RichText::new("Keys are stored separately for each provider, encrypted for this Windows account. No AI requests are made at startup. Save settings before recognizing an image.").small());
        if !self.notice.is_empty() {
            ui.colored_label(Color32::LIGHT_YELLOW, &self.notice);
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
    ) -> bool {
        self.poll();
        let mut save_requested = false;
        ui.horizontal(|ui| {
            ui.heading("Import from an image");
            if self
                .review
                .as_ref()
                .is_some_and(|r| !r.candidates.is_empty())
            {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Save selected subscriptions").clicked() {
                        save_requested = true;
                    }
                });
            }
        });
        ui.label("A screenshot or receipt can fill in your subscriptions for you.");
        ui.add_space(12.);
        let active = store.lock().unwrap().settings.ai.clone();
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "{} / {}",
                active.provider.label(),
                if active.active().model.is_empty() {
                    "Choose a vision model"
                } else {
                    &active.active().model
                }
            ));
        });
        let mut settings_requested = false;
        if ui.button("AI connection settings").clicked() {
            settings_requested = true;
        }
        ui.add_space(8.);
        let busy = self.busy();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!busy, egui::Button::new("Attach and recognize..."))
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
                    .pick_file()
                && self.load_file(&path, ctx)
            {
                self.recognize(store, ctx);
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Paste and recognize"))
                .clicked()
            {
                self.paste(store, ctx);
            }
            if ui
                .add_enabled(
                    !busy && self.attachment.is_some(),
                    egui::Button::new("Recognize again").fill(Color32::from_rgb(65, 89, 188)),
                )
                .clicked()
            {
                self.recognize(store, ctx);
            }
            if busy {
                ui.spinner();
                ui.label("Reading the image...");
            }
        });
        ui.label(RichText::new("Attach, drop, or paste a PNG, JPEG, or WebP to recognize it with your saved AI provider. The selected image is sent to that service. Review the result before saving.").small().color(Color32::from_rgb(153, 166, 190)));
        if !self.notice.is_empty() {
            ui.colored_label(Color32::LIGHT_YELLOW, &self.notice);
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            if let (Some(image), Some(texture)) = (&self.attachment, &self.texture) {
                ui.add_space(8.);
                ui.label(format!(
                    "{} · {} × {}",
                    image.name, image.width, image.height
                ));
                let width = ui.available_width().min(520.);
                let scale = (width / image.width as f32)
                    .min(150. / image.height as f32)
                    .min(1.);
                ui.image((
                    texture.id(),
                    egui::vec2(image.width as f32 * scale, image.height as f32 * scale),
                ));
            }
            if let Some(review) = &mut self.review {
                ui.add_space(12.);
                ui.label(
                    RichText::new(format!(
                        "Review {} detected subscription(s)",
                        review.candidates.len()
                    ))
                    .size(20.)
                    .strong(),
                );
                ui.label(RichText::new(format!("Recognized with {}", self.source)).small());
                if !review.warnings.is_empty() {
                    egui::CollapsingHeader::new(format!("Model notes ({})", review.warnings.len()))
                        .show(ui, |ui| {
                            for warning in &review.warnings {
                                ui.colored_label(Color32::LIGHT_YELLOW, warning);
                            }
                        });
                }
                for (index, item) in review.candidates.iter_mut().enumerate() {
                    ui.push_id(index, |ui| {
                        egui::Frame::group(ui.style())
                            .inner_margin(14)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut item.selected, "Include");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut item.sub.name)
                                            .hint_text("Subscription name")
                                            .desired_width(290.),
                                    );
                                });
                                ui.horizontal_wrapped(|ui| {
                                    ui.label("Expires");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut item.date)
                                            .hint_text("YYYY-MM-DD")
                                            .desired_width(135.),
                                    );
                                    ui.label("Price");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut item.sub.amount)
                                            .desired_width(75.),
                                    );
                                    ui.add(
                                        egui::TextEdit::singleline(&mut item.sub.currency)
                                            .hint_text("Currency")
                                            .desired_width(75.),
                                    );
                                    egui::ComboBox::from_id_salt("import-cycle")
                                        .selected_text(item.sub.cycle.label())
                                        .show_ui(ui, |ui| {
                                            for cycle in Cycle::ALL {
                                                ui.selectable_value(
                                                    &mut item.sub.cycle,
                                                    cycle,
                                                    cycle.label(),
                                                );
                                            }
                                        });
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Remind days before");
                                    ui.add(
                                        egui::DragValue::new(&mut item.sub.reminder_days)
                                            .range(0..=365),
                                    );
                                });
                                ui.add(
                                    egui::TextEdit::multiline(&mut item.sub.notes)
                                        .hint_text("Notes")
                                        .desired_rows(1)
                                        .desired_width(f32::INFINITY),
                                );
                            });
                    });
                }
                if !review.candidates.is_empty()
                    && ui.button("Save selected subscriptions").clicked()
                {
                    save_requested = true;
                }
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
                Err(e) => self.notice = e,
            }
        }
        settings_requested
    }
}
