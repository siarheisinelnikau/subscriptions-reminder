use crate::{
    model::{Delivery, reminder_text, was_delivered},
    platform::{Platform, unprotect},
    storage::{Settings, Store},
};
use chrono::Local;
use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    thread,
    time::Duration,
};

pub fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(25)))
        .build()
        .into()
}

pub fn send_telegram(settings: &Settings, text: &str) -> Result<(), String> {
    let token = unprotect(&settings.telegram_token_protected)?;
    if token.is_empty() || settings.telegram_chat_id.trim().is_empty() {
        return Err("Enter the Telegram bot token and chat ID in Settings.".into());
    }
    if !token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ':' || c == '_' || c == '-')
    {
        return Err("The Telegram bot token has an invalid format.".into());
    }
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");
    // Do not display transport errors: they can contain the secret token in the URL.
    let mut response = http_agent().post(&url).send_json(serde_json::json!({"chat_id": settings.telegram_chat_id.trim(), "text": text}))
        .map_err(|_| "Telegram delivery failed. Check the token, chat ID, network connection, and that you started the bot.".to_string())?;
    let json: serde_json::Value = response
        .body_mut()
        .read_json()
        .map_err(|_| "Telegram returned an invalid response".to_string())?;
    if json.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        return Err("Telegram did not accept the message. Check the bot and chat ID.".into());
    }
    Ok(())
}

pub fn start(store: Arc<Mutex<Store>>, platform: Arc<Platform>) {
    thread::spawn(move || {
        while !platform.quit.load(Ordering::SeqCst) {
            tick(&store, &platform);
            for _ in 0..30 {
                if platform.quit.load(Ordering::SeqCst) {
                    return;
                }
                thread::sleep(Duration::from_secs(1));
            }
        }
    });
}

fn tick(shared: &Arc<Mutex<Store>>, platform: &Platform) {
    let today = Local::now().date_naive();
    let (settings, due) = {
        let mut store = shared.lock().unwrap();
        if let Err(error) = store.refresh() {
            store.error = Some(error);
            return;
        }
        (
            store.settings.clone(),
            store
                .db
                .subscriptions
                .iter()
                .filter(|s| s.needs_reminder(today))
                .cloned()
                .collect::<Vec<_>>(),
        )
    };
    // Aggregate Windows reminders into one notification; Shell may replace rapid balloons.
    let windows: Vec<_> = due
        .iter()
        .filter(|s| {
            settings.windows_notifications && !was_delivered(&settings.deliveries, s, today, false)
        })
        .collect();
    if !windows.is_empty() {
        let text = if windows.len() == 1 {
            reminder_text(windows[0], today)
        } else {
            format!(
                "{} subscriptions need attention: {}. Open the app for details.",
                windows.len(),
                windows
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        match platform.notify("Subscription reminder".into(), text) {
            Ok(()) => {
                for sub in windows {
                    record(shared, sub.id, sub.due, today, false);
                }
            }
            Err(e) => shared.lock().unwrap().error = Some(e),
        }
    }
    if settings.telegram_enabled {
        for sub in &due {
            if platform.quit.load(Ordering::SeqCst) {
                break;
            }
            if was_delivered(&settings.deliveries, sub, today, true) {
                continue;
            }
            match send_telegram(&settings, &reminder_text(sub, today)) {
                Ok(()) => record(shared, sub.id, sub.due, today, true),
                Err(e) => {
                    shared.lock().unwrap().error = Some(e);
                    break;
                }
            }
        }
    }
}

fn record(
    shared: &Arc<Mutex<Store>>,
    id: uuid::Uuid,
    due: chrono::NaiveDate,
    today: chrono::NaiveDate,
    telegram: bool,
) {
    let mut store = shared.lock().unwrap();
    store
        .settings
        .deliveries
        .retain(|d| d.day.is_some_and(|day| (today - day).num_days() <= 35));
    if let Some(item) = store
        .settings
        .deliveries
        .iter_mut()
        .find(|d| d.id == id && d.due == Some(due) && d.day == Some(today))
    {
        if telegram {
            item.telegram = true;
        } else {
            item.windows = true;
        }
    } else {
        store.settings.deliveries.push(Delivery {
            id,
            due: Some(due),
            day: Some(today),
            windows: !telegram,
            telegram,
        });
    }
    if let Err(e) = store.save_settings() {
        store.error = Some(e);
    }
}
