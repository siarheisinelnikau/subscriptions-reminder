use crate::model::{Database, Delivery};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub drive: crate::drive::Settings,
    pub ai: crate::ai::Settings,
    pub data_dir: PathBuf,
    pub startup_update_check: bool,
    pub windows_notifications: bool,
    pub telegram_enabled: bool,
    pub telegram_token_protected: Vec<u8>,
    pub telegram_chat_id: String,
    pub deliveries: Vec<Delivery>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            drive: crate::drive::Settings::default(),
            ai: crate::ai::Settings::default(),
            data_dir: PathBuf::new(),
            startup_update_check: true,
            windows_notifications: true,
            telegram_enabled: false,
            telegram_token_protected: vec![],
            telegram_chat_id: String::new(),
            deliveries: vec![],
        }
    }
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes = fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| {
        format!(
            "Invalid JSON in {}: {e}. The original file was left untouched.",
            path.display()
        )
    })
}

pub fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path.parent().ok_or("Missing parent directory")?;
    fs::create_dir_all(parent).map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temp.write_all(&bytes)
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    if path.exists() {
        fs::copy(path, path.with_extension("json.bak"))
            .map_err(|e| format!("Cannot create backup: {e}"))?;
    }
    temp.persist(path)
        .map_err(|e| format!("Cannot save {}: {e}", path.display()))?;
    Ok(())
}

pub struct Store {
    pub config_dir: PathBuf,
    pub settings: Settings,
    pub db: Database,
    pub error: Option<String>,
    fingerprint: Option<Vec<u8>>,
}

impl Store {
    pub fn open(config_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
        let settings_path = config_dir.join("settings.json");
        let first_start = !settings_path.exists();
        let mut settings: Settings = if !first_start {
            read_json(&settings_path)?
        } else {
            Settings::default()
        };
        if settings.data_dir.as_os_str().is_empty() {
            settings.data_dir = config_dir.join("data");
        }
        let path = settings.data_dir.join("subscriptions.json");
        let db: Database = if path.exists() {
            read_json(&path)?
        } else if !first_start {
            return Err(format!(
                "The data file {} is unavailable. Check the selected data folder or wait for cloud sync. Your saved settings were left untouched.",
                path.display()
            ));
        } else {
            let db = Database::default();
            atomic_json(&path, &db)?;
            db
        };
        db.validate()?;
        let fingerprint = fs::read(&path).ok();
        let store = Self {
            config_dir,
            settings,
            db,
            error: None,
            fingerprint,
        };
        store.save_settings()?;
        Ok(store)
    }
    pub fn data_path(&self) -> PathBuf {
        self.settings.data_dir.join("subscriptions.json")
    }
    pub fn save_settings(&self) -> Result<(), String> {
        atomic_json(&self.config_dir.join("settings.json"), &self.settings)
    }
    /// Reload external changes and report whether the in-memory database changed.
    pub fn refresh(&mut self) -> Result<bool, String> {
        let path = self.data_path();
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && self.fingerprint.is_none() => {
                return Ok(false);
            }
            Err(e) => {
                return Err(format!(
                    "Cannot read {}: {e}. Data was not overwritten.",
                    path.display()
                ));
            }
        };
        if self.fingerprint.as_ref() != Some(&bytes) {
            let db: Database = serde_json::from_slice(&bytes)
                .map_err(|e| format!("External file is invalid: {e}. Data was not overwritten."))?;
            db.validate()?;
            self.db = db;
            self.fingerprint = Some(bytes);
            return Ok(true);
        }
        Ok(false)
    }
    pub fn commit(&mut self, db: Database) -> Result<(), String> {
        // Refuse a stale write if another application or sync client changed the file.
        if fs::read(self.data_path()).ok() != self.fingerprint {
            self.refresh()?;
            return Err("The file changed outside the application. Review the refreshed list and try again.".into());
        }
        db.validate()?;
        atomic_json(&self.data_path(), &db)?;
        self.fingerprint = Some(serde_json::to_vec_pretty(&db).map_err(|e| e.to_string())?);
        self.db = db;
        Ok(())
    }
    pub fn use_folder(&mut self, dir: PathBuf) -> Result<(), String> {
        if dir == self.settings.data_dir {
            return Ok(());
        }
        let path = dir.join("subscriptions.json");
        let db: Database = if path.exists() {
            read_json(&path)?
        } else {
            self.db.clone()
        };
        db.validate()?;
        if !path.exists() {
            atomic_json(&path, &db)?;
        }
        let old = self.settings.data_dir.clone();
        self.settings.data_dir = dir;
        if let Err(e) = self.save_settings() {
            self.settings.data_dir = old;
            return Err(e);
        }
        self.fingerprint = Some(fs::read(&path).map_err(|e| e.to_string())?);
        self.db = db;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Subscription;
    #[test]
    fn missing_synced_data_is_not_replaced_with_an_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().to_owned()).unwrap();
        let path = store.data_path();
        fs::remove_file(&path).unwrap();
        assert!(Store::open(dir.path().to_owned()).is_err());
        assert!(!path.exists());
    }
    #[test]
    fn saves_reopens_backs_up_and_rejects_external_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().to_owned()).unwrap();
        let mut db = store.db.clone();
        let mut sub = Subscription::new("2026-11-01".parse().unwrap());
        sub.name = "Test".into();
        db.subscriptions.push(sub);
        store.commit(db.clone()).unwrap();
        store.commit(db.clone()).unwrap();
        assert!(store.data_path().with_extension("json.bak").exists());
        let mut other = Store::open(dir.path().to_owned()).unwrap();
        assert!(!store.refresh().unwrap());
        db.subscriptions[0].name = "External".into();
        other.commit(db).unwrap();
        assert!(store.commit(store.db.clone()).is_err());
        assert_eq!(store.db.subscriptions[0].name, "External");
        assert!(!store.refresh().unwrap());
        let mut changed = other.db.clone();
        changed.subscriptions[0].name = "External again".into();
        other.commit(changed).unwrap();
        assert!(store.refresh().unwrap());
        assert!(!store.refresh().unwrap());
        fs::write(store.data_path(), b"broken").unwrap();
        assert!(store.refresh().is_err());
        assert!(store.commit(store.db.clone()).is_err());
        assert_eq!(fs::read(store.data_path()).unwrap(), b"broken");
    }
    #[test]
    fn folder_switch_loads_existing_data_without_overwriting_it() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().to_owned()).unwrap();
        let db = Database::default();
        atomic_json(&other.path().join("subscriptions.json"), &db).unwrap();
        store.use_folder(other.path().to_owned()).unwrap();
        assert_eq!(store.settings.data_dir, other.path());
        assert!(
            Store::open(dir.path().to_owned())
                .unwrap()
                .db
                .subscriptions
                .is_empty()
        );
    }
}
