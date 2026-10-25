//! Product settings and UI dispatch for the shared portable updater.
use crate::platform::Platform;
use rust_auto_updator::{Config, Release};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn config(config_dir: Option<PathBuf>) -> Result<Config, String> {
    let mut config = Config::new(
        "siarheisinelnikau",
        "subscriptions-reminder",
        VERSION,
        "SubscriptionsReminder",
    )?;
    config.max_executable_size = 200 * 1024 * 1024;
    config.expected_subsystem = Some(2); // Windows GUI executable.
    config.asset_suffixes = vec!["-x64".into()];
    config.install.stage_prefix = ".subscriptions-update-".into();
    if let Some(directory) = config_dir {
        config.install.restart_args = vec![
            "--data-dir".into(),
            directory.to_string_lossy().into_owned(),
        ];
        config.install.rollback_args = config.install.restart_args.clone();
    }
    Ok(config)
}

#[derive(Clone, Default)]
pub enum State {
    #[default]
    Idle,
    Checking,
    Current,
    Available(Box<Release>),
    Downloading(f32),
    Error(String),
}
pub type Shared = Arc<Mutex<State>>;

pub fn check(state: Shared, ctx: egui::Context) {
    {
        let mut value = state.lock().unwrap();
        if matches!(*value, State::Checking | State::Downloading(_)) {
            return;
        }
        *value = State::Checking;
    }
    thread::spawn(move || {
        let result = config(None).and_then(|config| rust_auto_updator::check_latest(&config));
        *state.lock().unwrap() = match result {
            Ok(Some(release)) => State::Available(Box::new(release)),
            Ok(None) => State::Current,
            Err(error) => State::Error(error),
        };
        ctx.request_repaint();
    });
}

pub fn install(
    release: Box<Release>,
    state: Shared,
    ctx: egui::Context,
    platform: Arc<Platform>,
    config_dir: PathBuf,
) {
    {
        let mut value = state.lock().unwrap();
        if matches!(*value, State::Checking | State::Downloading(_)) {
            return;
        }
        *value = State::Downloading(0.0);
    }
    thread::spawn(move || {
        let result = (|| {
            let config = config(Some(config_dir))?;
            let mut prepared =
                rust_auto_updator::prepare_download(&release, &config, |done, total| {
                    *state.lock().unwrap() = State::Downloading(done as f32 / total.max(1) as f32);
                    ctx.request_repaint();
                })?;
            rust_auto_updator::handoff(&mut prepared)
        })();
        match result {
            Ok(()) => platform.request_quit(),
            Err(error) => {
                *state.lock().unwrap() = State::Error(error);
                ctx.request_repaint();
            }
        }
    });
}

pub fn apply_update() -> Result<(), String> {
    rust_auto_updator::apply_update(&config(None)?)
}

pub fn cleanup(directory: PathBuf) -> Result<(), String> {
    rust_auto_updator::cleanup_after_update(directory, config(None)?.install);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn released_x64_names_and_data_directory_survive_migration() {
        let directory = PathBuf::from(r"E:\Fixture Data\Subscriptions");
        let mut config = config(Some(directory.clone())).unwrap();
        config.current_version = "0.1.0".parse().unwrap();
        let base = config.repository_url();
        let name = "SubscriptionsReminder-v0.6.0-x64.exe";
        let release = serde_json::json!({"tag_name":"v0.6.0", "html_url":format!("{base}/releases/tag/v0.6.0"),
            "draft":false,"prerelease":false,"assets":[
                {"name":name,"size":2048,"browser_download_url":format!("{base}/releases/download/v0.6.0/{name}")},
                {"name":"SHA256SUMS.txt","size":128,"browser_download_url":format!("{base}/releases/download/v0.6.0/SHA256SUMS.txt")}]});
        let parsed = rust_auto_updator::parse_release(&release.to_string(), &config)
            .unwrap()
            .unwrap();
        assert_eq!(parsed.executable_name(), name);
        assert_eq!(
            config.install.restart_args,
            vec!["--data-dir", directory.to_str().unwrap()]
        );
        let mut untrusted = release;
        untrusted["assets"][0]["browser_download_url"] = "https://example.com/app.exe".into();
        assert!(rust_auto_updator::parse_release(&untrusted.to_string(), &config).is_err());
    }
}
