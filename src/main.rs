#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod capture;
mod drive;
mod event_loop;
mod import_ui;
mod model;
mod platform;
mod reminders;
mod storage;
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
        if let Err(e) = updates::apply_update(&args) {
            platform::show_error(&e);
        }
        return;
    }
    if let Err(error) = run(&args) {
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
        image: argument_path("--image")?,
        recognize: args.iter().any(|s| s == "--recognize"),
    };
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
    let store = Arc::new(Mutex::new(store));
    let platform = platform::Platform::start(identity)?;
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
