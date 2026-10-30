use eframe::egui;
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
            0 => 0,
            1 | 2 => 2,
            _ => 1,
        }
    }
    pub fn bottom(&self) -> bool {
        self.step == 2
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
            if self.step == 3
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
                "storage-updates.png",
                "image-import.png",
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
            if self.step == 4 {
                self.done = true;
                return Ok(());
            }
        }
        if self.wait > 0 {
            self.wait -= 1;
            ctx.request_repaint();
        } else if !(self.requested || self.step == 3 && busy) {
            self.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            ctx.request_repaint();
        }
        Ok(())
    }
}
