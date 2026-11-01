#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod capture;
mod drive;
mod event_loop;
mod i18n;
#[cfg(test)]
mod icons;
mod import_ui;
mod model;
mod platform;
mod reminders;
mod storage;
mod theme;
mod ui;
mod updates;

use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--apply-update") {
        if let Err(e) = updates::apply_update() {
            platform::show_error(&e);
        }
        return;
    }
    if let Err(error) = run(&args) {
        if args.iter().any(|arg| arg == "--diagnostics") {
            eprintln!("Subscriptions Reminder diagnostic: {error}");
            std::process::exit(1);
        }
        platform::show_error(&error);
    }
}

fn run(args: &[String]) -> Result<(), String> {
    if args.iter().any(|s| s == "--demo") && !args.iter().any(|s| s == "--data-dir") {
        return Err(
            "Use --data-dir with --demo to keep sample data separate from your subscriptions."
                .into(),
        );
    }
    let config_dir = if let Some(index) = args.iter().position(|s| s == "--data-dir") {
        PathBuf::from(
            args.get(index + 1)
                .ok_or("--data-dir requires a directory")?,
        )
    } else {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not available")?)
            .join("SubscriptionsReminder")
    };
    let argument_path = |flag: &str| -> Result<Option<PathBuf>, String> {
        args.iter()
            .position(|s| s == flag)
            .map(|i| {
                args.get(i + 1)
                    .map(PathBuf::from)
                    .ok_or_else(|| format!("{flag} requires a path"))
            })
            .transpose()
    };
    let launch = ui::Launch {
        capture_dir: argument_path("--capture-dir")?,
        diagnostic: argument_path("--diagnostics")?,
        image: argument_path("--image")?,
        recognize: args.iter().any(|s| s == "--recognize"),
    };
    if launch.diagnostic.is_some()
        && (!args.iter().any(|s| s == "--data-dir") || !args.iter().any(|s| s == "--demo"))
    {
        return Err("Diagnostics require --demo and an explicit isolated --data-dir.".into());
    }
    if launch.capture_dir.is_some()
        && (!args.iter().any(|s| s == "--data-dir") || !args.iter().any(|s| s == "--demo"))
    {
        return Err("Capture requires --demo and an explicit isolated --data-dir.".into());
    }
    fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    let config_dir = config_dir.canonicalize().map_err(|e| e.to_string())?;
    let identity = format!(
        "SubscriptionsReminder-{:x}",
        Sha256::digest(config_dir.to_string_lossy().to_lowercase().as_bytes())
    );
    let instance = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(config_dir.join("instance.lock"))
        .map_err(|e| e.to_string())?;
    if instance.try_lock_exclusive().is_err() {
        if !platform::open_existing(&identity) {
            return Err(
                "Subscriptions Reminder is already starting. Try opening it again in a moment."
                    .into(),
            );
        }
        return Ok(());
    }
    let mut store = storage::Store::open(config_dir)?;
    if args.iter().any(|s| s == "--demo") && store.db.subscriptions.is_empty() {
        let today = chrono::Local::now().date_naive();
        let mut db = model::Database::default();
        for (name, days, price, notes) in [
            ("Cloud backup", 2, "4.99", "Personal files · 200 GB plan"),
            ("Music subscription", 9, "10.99", "Individual plan"),
            (
                "Design workspace",
                -1,
                "15.00",
                "Review the plan before renewing",
            ),
            ("Password manager", 3, "2.99", "Family account"),
            ("Video library", 5, "8.99", "Standard plan"),
            ("VPN service", 12, "5.00", ""),
            ("Domain renewal", 17, "12.00", "Personal website"),
            ("Language course", 21, "7.50", "Monthly lessons"),
            ("Reading app", 28, "9.99", ""),
            ("Developer tools", 30, "12.00", "Personal workspace"),
            ("Fitness plan", 35, "14.99", ""),
            (
                "Family photo archive and cloud storage",
                45,
                "3.99",
                "Shared library and automatic photo backups",
            ),
        ] {
            let mut sub = model::Subscription::new(today + chrono::Duration::days(days));
            sub.name = name.into();
            sub.amount = price.into();
            sub.notes = notes.into();
            db.subscriptions.push(sub);
        }
        store.commit(db)?;
        store.settings.windows_notifications = false;
        store.settings.startup_update_check = false;
        store.save_settings()?;
    }
    if launch.capture_dir.is_some() {
        // Capture choices affect only the isolated fixture, never ordinary settings.
        if let Some(index) = args.iter().position(|s| s == "--capture-language") {
            store.settings.language = match args.get(index + 1).map(String::as_str) {
                Some("en") => i18n::Language::English,
                Some("ru") => i18n::Language::Russian,
                Some("fr") => i18n::Language::French,
                Some("de") => i18n::Language::German,
                Some("es") => i18n::Language::Spanish,
                Some("it") => i18n::Language::Italian,
                _ => {
                    return Err("Unknown --capture-language; use en, ru, fr, de, es, or it.".into());
                }
            };
        }
        if let Some(index) = args.iter().position(|s| s == "--capture-theme") {
            store.settings.theme = match args.get(index + 1).map(String::as_str) {
                Some("light") => theme::Preference::Light,
                Some("dark") => theme::Preference::Dark,
                Some("system") => theme::Preference::System,
                _ => return Err("Unknown --capture-theme; use light, dark, or system.".into()),
            };
        }
        store.settings.windows_notifications = false;
        store.settings.telegram_enabled = false;
        store.settings.startup_update_check = false;
        store.settings.drive = Default::default();
        store.settings.ai.enabled = false;
    }
    store.settings.language.select();
    let store = Arc::new(Mutex::new(store));
    let platform = platform::Platform::start(identity)?;
    if let Some(index) = args.iter().position(|s| s == "--cleanup-update") {
        let directory = args
            .get(index + 1)
            .ok_or("--cleanup-update requires a directory")?;
        updates::cleanup(PathBuf::from(directory))?;
    }
    reminders::start(store.clone(), platform.clone());
    let stopped = platform.clone();
    let result = event_loop::run(
        store,
        platform,
        launch,
        args.iter().any(|s| s == "--background"),
    );
    stopped.stop();
    drop(instance);
    result
}
