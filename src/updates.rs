use crate::{platform::Platform, reminders::http_agent};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    thread,
};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};

pub const REPOSITORY: &str = "siarheisinelnikau/subscriptions-reminder";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}
#[derive(Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub draft: bool,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}
#[derive(Clone, Default)]
pub enum State {
    #[default]
    Idle,
    Checking,
    Current,
    Available(Release),
    Downloading(f32),
    Error(String),
}
pub type Shared = Arc<Mutex<State>>;

pub fn compatible_asset(release: &Release) -> Result<&Asset, String> {
    if release.draft || release.prerelease {
        return Err("Only stable published releases are supported.".into());
    }
    let version = Version::parse(release.tag_name.trim_start_matches('v'))
        .map_err(|_| "The release version is invalid".to_string())?;
    if !version.pre.is_empty() {
        return Err("Prerelease versions are excluded from stable updates.".into());
    }
    let names = [
        format!("SubscriptionsReminder-v{version}.exe"),
        format!("SubscriptionsReminder-v{version}-x64.exe"),
    ];
    release
        .assets
        .iter()
        .find(|a| names.contains(&a.name))
        .ok_or_else(|| "This release has no compatible Windows x64 executable.".into())
}

fn trusted_url(url: &str) -> bool {
    url.starts_with(&format!(
        "https://github.com/{REPOSITORY}/releases/download/"
    ))
}

pub fn check(state: Shared, ctx: eframe::egui::Context) {
    {
        let mut value = state.lock().unwrap();
        if matches!(*value, State::Checking | State::Downloading(_)) {
            return;
        }
        *value = State::Checking;
    }
    thread::spawn(move || {
        let result = (|| -> Result<State, String> {
            let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
            let response = http_agent()
                .get(&url)
                .header(
                    "User-Agent",
                    concat!("SubscriptionsReminder/", env!("CARGO_PKG_VERSION")),
                )
                .header("Accept", "application/vnd.github+json")
                .call();
            let mut response = match response {
                Err(ureq::Error::StatusCode(404)) => return Ok(State::Current),
                Err(ureq::Error::StatusCode(403 | 429)) => {
                    return Err("GitHub rate limit reached. Try checking again later.".into());
                }
                Err(e) => return Err(format!("Could not check GitHub for updates: {e}")),
                Ok(r) => r,
            };
            let release: Release = response
                .body_mut()
                .read_json()
                .map_err(|e| format!("Invalid release response: {e}"))?;
            let remote = Version::parse(release.tag_name.trim_start_matches('v'))
                .map_err(|_| "Invalid release version")?;
            if !remote.pre.is_empty()
                || release.draft
                || release.prerelease
                || remote <= Version::parse(VERSION).unwrap()
            {
                return Ok(State::Current);
            }
            compatible_asset(&release)?;
            Ok(State::Available(release))
        })();
        *state.lock().unwrap() = result.unwrap_or_else(State::Error);
        ctx.request_repaint();
    });
}

pub fn install(
    release: Release,
    state: Shared,
    ctx: eframe::egui::Context,
    platform: Arc<Platform>,
    config_dir: PathBuf,
) {
    {
        let mut value = state.lock().unwrap();
        if matches!(*value, State::Downloading(_)) {
            return;
        }
        *value = State::Downloading(0.0);
    }
    thread::spawn(move || {
        let result = download_and_prepare(&release, &state, &ctx, &config_dir);
        match result {
            Ok(()) => platform.request_quit(),
            Err(e) => {
                *state.lock().unwrap() = State::Error(e);
                ctx.request_repaint();
            }
        }
    });
}

fn download_and_prepare(
    release: &Release,
    state: &Shared,
    ctx: &eframe::egui::Context,
    config_dir: &Path,
) -> Result<(), String> {
    let asset = compatible_asset(release)?;
    let checksum = release
        .assets
        .iter()
        .find(|a| a.name == "SHA256SUMS.txt")
        .ok_or("The release is missing SHA256SUMS.txt. Installation was stopped.")?;
    if !trusted_url(&asset.browser_download_url) || !trusted_url(&checksum.browser_download_url) {
        return Err("The update URL does not belong to the official repository.".into());
    }
    if asset.size < 1024 || asset.size > 200 * 1024 * 1024 {
        return Err("The update has an unexpected size.".into());
    }
    let agent = http_agent();
    let mut response = agent
        .get(&checksum.browser_download_url)
        .call()
        .map_err(|e| format!("Cannot download checksums: {e}"))?;
    let sums = response
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let expected = checksum_for(&sums, &asset.name)?;
    let dir = tempfile::Builder::new()
        .prefix("SubscriptionsReminder-update-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let download = dir.path().join("download.exe");
    let mut response = agent
        .get(&asset.browser_download_url)
        .call()
        .map_err(|e| format!("Cannot download update: {e}"))?;
    let mut reader = response.body_mut().as_reader();
    let mut file = fs::File::create(&download).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|e| format!("Update download interrupted: {e}"))?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > asset.size {
            return Err("The update is larger than the published asset.".into());
        }
        hash.update(&buffer[..count]);
        file.write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
        *state.lock().unwrap() = State::Downloading(total as f32 / asset.size as f32);
        ctx.request_repaint();
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    if total != asset.size || format!("{:x}", hash.finalize()) != expected {
        return Err(
            "The update failed its size or SHA-256 check. Your installation was not changed."
                .into(),
        );
    }
    let target = std::env::current_exe().map_err(|e| e.to_string())?;
    let helper = dir.path().join("updater.exe");
    fs::copy(&target, &helper).map_err(|e| format!("Cannot prepare updater: {e}"))?;
    Command::new(&helper)
        .arg("--apply-update")
        .arg(std::process::id().to_string())
        .arg(&target)
        .arg(&download)
        .arg(config_dir)
        .spawn()
        .map_err(|e| format!("Cannot start updater: {e}"))?;
    // The helper owns the temporary files after the application exits.
    let _ = dir.keep();
    Ok(())
}

fn checksum_for(sums: &str, name: &str) -> Result<String, String> {
    for line in sums.lines() {
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() == 2
            && parts[1].trim_start_matches('*') == name
            && parts[0].len() == 64
            && parts[0].bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Ok(parts[0].to_ascii_lowercase());
        }
    }
    Err("No valid SHA-256 checksum was published for this executable.".into())
}

pub fn apply_update(args: &[String]) -> Result<(), String> {
    if args.len() != 6 {
        return Err("Invalid updater arguments".into());
    }
    let pid: u32 = args[2].parse().map_err(|_| "Invalid parent process")?;
    let target = PathBuf::from(&args[3]);
    let download = PathBuf::from(&args[4]);
    let config = PathBuf::from(&args[5]);
    if !target.is_absolute()
        || target.extension().is_none_or(|e| e != "exe")
        || download.file_name().is_none_or(|f| f != "download.exe")
    {
        return Err("Invalid update paths".into());
    }
    unsafe {
        let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if !process.is_null() {
            let status = WaitForSingleObject(process, 120_000);
            CloseHandle(process);
            if status != 0 {
                return Err("The application did not exit. Close it and try again.".into());
            }
        }
    }
    let incoming = target.with_extension(format!("{}.new.exe", uuid::Uuid::new_v4()));
    let backup = target.with_extension(format!("{}.old.exe", uuid::Uuid::new_v4()));
    fs::copy(&download, &incoming).map_err(|e| format!("Cannot write to the application folder: {e}. Move the app to a writable folder and try again."))?;
    if let Err(e) = fs::rename(&target, &backup) {
        let _ = fs::remove_file(&incoming);
        return Err(format!("Cannot replace the running application: {e}"));
    }
    if let Err(e) = fs::rename(&incoming, &target) {
        let _ = fs::rename(&backup, &target);
        return Err(format!(
            "Cannot install update: {e}. The previous version was restored."
        ));
    }
    if let Err(e) = Command::new(&target).arg("--data-dir").arg(config).spawn() {
        let _ = fs::remove_file(&target);
        let _ = fs::rename(&backup, &target);
        return Err(format!(
            "Could not restart the update: {e}. The previous version was restored."
        ));
    }
    let _ = fs::remove_file(&backup);
    let _ = fs::remove_file(&download);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_versions_assets_and_checksum_are_checked() {
        assert!(Version::parse("0.10.0").unwrap() > Version::parse("0.9.9").unwrap());
        let mut release = Release {
            tag_name: "v0.2.0".into(),
            draft: false,
            prerelease: false,
            assets: vec![Asset {
                name: "SubscriptionsReminder-v0.2.0.exe".into(),
                browser_download_url: String::new(),
                size: 2048,
            }],
        };
        assert!(compatible_asset(&release).is_ok());
        release.prerelease = true;
        assert!(compatible_asset(&release).is_err());
        release.prerelease = false;
        release.assets[0].name = "SubscriptionsReminder-v0.2.0-arm64.exe".into();
        assert!(compatible_asset(&release).is_err());
        let digest = "a".repeat(64);
        assert_eq!(
            checksum_for(&format!("{digest}  app.exe\n"), "app.exe").unwrap(),
            digest
        );
        assert!(checksum_for("broken app.exe", "app.exe").is_err());
        assert!(trusted_url(
            "https://github.com/siarheisinelnikau/subscriptions-reminder/releases/download/v0.1.0/app.exe"
        ));
        assert!(!trusted_url(
            "https://github.com/attacker/releases/download/app.exe"
        ));
    }
}
