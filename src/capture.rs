use std::path::PathBuf;

pub struct Capture {
    dir: PathBuf,
    step: usize,
    wait: u8,
    requested: bool,
    pub done: bool,
}
impl Capture {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            step: 0,
            wait: 5,
            requested: false,
            done: false,
        }
    }
    pub fn page(&self) -> usize {
        match self.step {
            0 | 7 => 0,
            1..=5 => 2,
            _ => 1,
        }
    }
    pub fn settings_tab(&self) -> Option<usize> {
        match self.step {
            1..=3 => Some(self.step - 1),
            4 => Some(4),
            5 => Some(3),
            _ => None,
        }
    }
    pub fn editor(&self) -> bool {
        self.step == 7
    }
    pub fn tick(
        &mut self,
        ctx: &egui::Context,
        busy: bool,
        recognition: Option<&crate::ai::Recognition>,
    ) -> Result<(), String> {
        if self.done {
            return Ok(());
        }
        let image = ctx.input(|i| {
            i.events.iter().find_map(|event| {
                if let egui::Event::Screenshot { image, .. } = event {
                    Some(image.clone())
                } else {
                    None
                }
            })
        });
        if let Some(image) = image {
            std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
            if self.step == 6
                && let Some(result) = recognition
            {
                std::fs::write(
                    self.dir.join("recognition.json"),
                    serde_json::to_vec_pretty(result).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
            }
            let name = [
                "subscriptions.png",
                "settings.png",
                "ai-settings.png",
                "notifications.png",
                "storage-updates.png",
                "google-drive.png",
                "image-import.png",
                "editor.png",
            ][self.step];
            let bytes = image
                .pixels
                .iter()
                .flat_map(|p| p.to_array())
                .collect::<Vec<u8>>();
            image::save_buffer(
                self.dir.join(name),
                &bytes,
                image.width() as u32,
                image.height() as u32,
                image::ColorType::Rgba8,
            )
            .map_err(|e| e.to_string())?;
            self.step += 1;
            self.wait = 5;
            self.requested = false;
            if self.step == 8 {
                self.done = true;
                return Ok(());
            }
        }
        if self.wait > 0 {
            self.wait -= 1;
            ctx.request_repaint();
        } else if !(self.requested || self.step == 6 && busy) {
            self.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            ctx.request_repaint();
        }
        Ok(())
    }
}
