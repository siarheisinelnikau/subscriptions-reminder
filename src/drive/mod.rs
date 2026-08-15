mod api;
mod oauth;
pub mod replica;
#[cfg(test)]
mod test_support;
pub mod ui;

use crate::{
    model::Database,
    platform::{self, Platform},
    storage::{self, Store},
};
use api::{Backend, Google};
use chrono::{DateTime, Utc};
use replica::{Conflict, Replica, Resolution};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub client_id: String,
    pub client_secret_protected: Vec<u8>,
    pub refresh_token_protected: Vec<u8>,
    pub account_id: String,
    pub account_label: String,
    pub auto_sync: bool,
    pub needs_sign_in: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret_protected: vec![],
            refresh_token_protected: vec![],
            account_id: String::new(),
            account_label: String::new(),
            auto_sync: true,
            needs_sign_in: false,
        }
    }
}
impl Settings {
    pub fn connected(&self) -> bool {
        !self.refresh_token_protected.is_empty() && !self.account_id.is_empty()
    }
    fn client(&self) -> Result<oauth::Client, Failure> {
        if self.client_id.is_empty() {
            return Err(Failure::data(
                "Import a Google Desktop OAuth client JSON before connecting.",
            ));
        }
        Ok(oauth::Client {
            id: self.client_id.clone(),
            secret: platform::unprotect(&self.client_secret_protected).map_err(Failure::data)?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Failure {
    pub message: String,
    pub sign_in: bool,
    pub access_expired: bool,
}
impl Failure {
    fn data(message: impl AsRef<str>) -> Self {
        Self {
            message: message.as_ref().into(),
            sign_in: false,
            access_expired: false,
        }
    }
    fn retry(message: impl AsRef<str>) -> Self {
        Self::data(message)
    }
    fn sign_in(message: impl AsRef<str>) -> Self {
        Self {
            sign_in: true,
            ..Self::data(message)
        }
    }
    fn access_expired(message: impl AsRef<str>) -> Self {
        Self {
            access_expired: true,
            ..Self::data(message)
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Checkpoint {
    format_version: u32,
    device: Uuid,
    file_id: Option<String>,
    uploaded_hash: Option<String>,
    replica: Replica,
    materialized: Database,
    checksums: BTreeMap<String, String>,
    last_success: Option<DateTime<Utc>>,
    #[serde(skip)]
    path: PathBuf,
}
impl Checkpoint {
    fn open(store: &Store) -> Result<Self, Failure> {
        let identity = format!(
            "{}\n{}\n{}",
            store.settings.drive.client_id,
            store.settings.drive.account_id,
            store.data_path().display()
        );
        let path = store
            .config_dir
            .join("drive")
            .join(format!("{:x}.json", Sha256::digest(identity.as_bytes())));
        let mut value: Self = if path.exists() {
            storage::read_json(&path).map_err(Failure::data)?
        } else {
            Self {
                format_version: 1,
                device: Uuid::new_v4(),
                file_id: None,
                uploaded_hash: None,
                replica: Replica::default(),
                materialized: Database::default(),
                checksums: BTreeMap::new(),
                last_success: None,
                path: path.clone(),
            }
        };
        if value.format_version != 1 || value.device.is_nil() {
            return Err(Failure::data(
                "Unsupported or invalid local Google Drive sync state. Local subscriptions were preserved.",
            ));
        }
        value.replica.validate().map_err(Failure::data)?;
        value.materialized.validate().map_err(Failure::data)?;
        value.path = path;
        Ok(value)
    }
    fn save(&self) -> Result<(), Failure> {
        storage::atomic_json(&self.path, self).map_err(Failure::data)
    }
    fn capture_local(&mut self, store: &mut Store) -> Result<bool, Failure> {
        store.refresh().map_err(Failure::data)?;
        let changed = self
            .replica
            .observe_local(&self.materialized, &store.db, self.device)
            .map_err(Failure::data)?;
        if changed {
            self.materialized = store.db.clone();
            self.save()?;
        }
        Ok(changed)
    }
}

fn state_hash(state: &Replica) -> Result<String, Failure> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(state).map_err(|_| Failure::data("Cannot encode sync state."))?
        )
    ))
}

fn synchronize(
    shared: &Arc<Mutex<Store>>,
    checkpoint: &mut Checkpoint,
    backend: &impl Backend,
) -> Result<(), Failure> {
    let initial = {
        let mut store = shared.lock().unwrap();
        checkpoint.capture_local(&mut store)?;
        store.db.clone()
    };
    let mut work = checkpoint.clone();
    let files = backend.list()?;
    for file in &files {
        if file
            .checksum
            .as_ref()
            .is_some_and(|sum| work.checksums.get(&file.id) == Some(sum))
        {
            continue;
        }
        work.replica
            .merge(&backend.download(&file.id)?)
            .map_err(Failure::data)?;
        if let Some(checksum) = &file.checksum {
            work.checksums.insert(file.id.clone(), checksum.clone());
        }
    }
    {
        let mut store = shared.lock().unwrap();
        store.refresh().map_err(Failure::data)?;
        // Edits made while the network was in flight use the previously observed
        // causal context, so an unseen remote edit remains a concurrent branch.
        let mut local = checkpoint.replica.clone();
        local
            .observe_local(&initial, &store.db, checkpoint.device)
            .map_err(Failure::data)?;
        work.replica.merge(&local).map_err(Failure::data)?;
        let merged = work.replica.materialize(&store.db);
        if merged != store.db {
            store.commit(merged).map_err(Failure::data)?;
        }
        work.materialized = store.db.clone();
        *checkpoint = work;
        checkpoint.save()?;
    }
    let hash = state_hash(&checkpoint.replica)?;
    let listed = checkpoint
        .file_id
        .as_ref()
        .is_some_and(|id| files.iter().any(|file| &file.id == id));
    let exists = if listed {
        true
    } else if let Some(id) = &checkpoint.file_id {
        backend.exists(id)?
    } else {
        false
    };
    if !exists && checkpoint.uploaded_hash.is_some() {
        checkpoint.file_id = None;
        checkpoint.uploaded_hash = None;
        checkpoint.save()?;
    }
    if checkpoint.uploaded_hash.as_ref() != Some(&hash) || !exists {
        if checkpoint.file_id.is_none() {
            checkpoint.file_id = Some(backend.generate_id()?);
            checkpoint.save()?;
        }
        let id = checkpoint.file_id.as_ref().unwrap().clone();
        let file = backend.upload(&id, checkpoint.device, &checkpoint.replica, !exists)?;
        checkpoint.uploaded_hash = Some(hash);
        if let Some(checksum) = file.checksum {
            checkpoint.checksums.insert(file.id, checksum);
        }
    }
    checkpoint.last_success = Some(Utc::now());
    checkpoint.save()?;
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
pub enum Phase {
    Setup,
    Disconnected,
    Authorizing,
    Syncing,
    Connected,
    Offline,
    SignInRequired,
}
#[derive(Clone)]
pub struct Status {
    pub phase: Phase,
    pub message: String,
    pub browser_url: Option<String>,
    pub last_success: Option<DateTime<Utc>>,
    pub conflicts: Vec<Conflict>,
    pub pending: bool,
}
impl Default for Status {
    fn default() -> Self {
        Self {
            phase: Phase::Disconnected,
            message: "Google Drive is not connected.".into(),
            browser_url: None,
            last_success: None,
            conflicts: vec![],
            pending: false,
        }
    }
}

enum Command {
    Import(oauth::Client),
    Connect,
    Disconnect,
    Sync,
    Pause,
    Resolve(Conflict, Resolution),
}
pub struct Controller {
    sender: mpsc::Sender<Command>,
    status: Arc<Mutex<Status>>,
    cancel: Arc<AtomicBool>,
}
impl Controller {
    pub fn new(
        shared: Arc<Mutex<Store>>,
        platform: Arc<Platform>,
        ctx: eframe::egui::Context,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_status = status.clone();
        let worker_cancel = cancel.clone();
        thread::spawn(move || {
            worker(
                shared,
                platform,
                ctx,
                receiver,
                worker_status,
                worker_cancel,
            )
        });
        Self {
            sender,
            status,
            cancel,
        }
    }
    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }
    pub fn import(&self, bytes: &[u8]) -> Result<(), String> {
        let client = oauth::Client::parse(bytes)?;
        self.sender
            .send(Command::Import(client))
            .map_err(|_| "The sync worker stopped. Restart the application.".into())
    }
    pub fn connect(&self) {
        let _ = self.sender.send(Command::Connect);
    }
    pub fn disconnect(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        let _ = self.sender.send(Command::Disconnect);
    }
    pub fn cancel_sign_in(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
    pub fn sync_now(&self) {
        let _ = self.sender.send(Command::Sync);
    }
    pub fn pause(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        let _ = self.sender.send(Command::Pause);
    }
    pub fn resolve(&self, conflict: Conflict, choice: Resolution) {
        let _ = self.sender.send(Command::Resolve(conflict, choice));
    }
}

struct Access {
    token: String,
    expires: Instant,
}
fn access(shared: &Arc<Mutex<Store>>, cache: &mut Option<Access>) -> Result<String, Failure> {
    if let Some(token) = cache
        && token.expires > Instant::now() + Duration::from_secs(60)
    {
        return Ok(token.token.clone());
    }
    let settings = shared.lock().unwrap().settings.drive.clone();
    let refresh = platform::unprotect(&settings.refresh_token_protected).map_err(Failure::data)?;
    let token = oauth::refresh(&settings.client()?, &refresh)?;
    if let Some(refresh) = &token.refresh {
        let protected = platform::protect(refresh).map_err(Failure::data)?;
        let mut store = shared.lock().unwrap();
        let old = store.settings.drive.refresh_token_protected.clone();
        store.settings.drive.refresh_token_protected = protected;
        if let Err(error) = store.save_settings() {
            store.settings.drive.refresh_token_protected = old;
            return Err(Failure::data(error));
        }
    }
    let text = token.access.clone();
    *cache = Some(Access {
        token: token.access,
        expires: Instant::now() + Duration::from_secs(token.expires_in),
    });
    Ok(text)
}

fn update_status(
    status: &Arc<Mutex<Status>>,
    ctx: &eframe::egui::Context,
    phase: Phase,
    message: impl Into<String>,
) {
    let message = message.into();
    let mut status = status.lock().unwrap();
    if status.phase == phase && status.message == message {
        return;
    }
    status.phase = phase;
    status.message = message;
    if status.phase != Phase::Authorizing {
        status.browser_url = None;
    }
    drop(status);
    ctx.request_repaint();
}
fn save_settings(shared: &Arc<Mutex<Store>>, next: Settings) -> Result<(), Failure> {
    let mut store = shared.lock().unwrap();
    let previous = store.settings.drive.clone();
    store.settings.drive = next;
    if let Err(error) = store.save_settings() {
        store.settings.drive = previous;
        return Err(Failure::data(error));
    }
    Ok(())
}
fn connect(
    shared: &Arc<Mutex<Store>>,
    platform: &Platform,
    ctx: &eframe::egui::Context,
    status: &Arc<Mutex<Status>>,
    cancel: &AtomicBool,
) -> Result<Access, Failure> {
    let mut settings = shared.lock().unwrap().settings.drive.clone();
    let client = settings.client()?;
    cancel.store(false, Ordering::SeqCst);
    update_status(
        status,
        ctx,
        Phase::Authorizing,
        "Finish Google sign-in in your browser. The app remains usable.",
    );
    let token = oauth::sign_in(&client, cancel, &platform.quit, |url| {
        status.lock().unwrap().browser_url = Some(url);
        ctx.request_repaint();
    })?;
    if cancel.load(Ordering::SeqCst) {
        return Err(Failure::data("Google sign-in was cancelled."));
    }
    let refresh = token.refresh.ok_or_else(|| {
        Failure::sign_in(
            "Google returned no offline session. Connect again and accept the consent screen.",
        )
    })?;
    let account = Google::new(token.access.clone()).account()?;
    if cancel.load(Ordering::SeqCst) {
        return Err(Failure::data("Google sign-in was cancelled."));
    }
    settings.refresh_token_protected = platform::protect(&refresh).map_err(Failure::data)?;
    settings.account_id = account.id;
    settings.account_label = if account.email.is_empty() {
        account.name
    } else {
        account.email
    };
    settings.needs_sign_in = false;
    save_settings(shared, settings)?;
    Ok(Access {
        token: token.access,
        expires: Instant::now() + Duration::from_secs(token.expires_in),
    })
}

fn worker(
    shared: Arc<Mutex<Store>>,
    platform: Arc<Platform>,
    ctx: eframe::egui::Context,
    receiver: mpsc::Receiver<Command>,
    status: Arc<Mutex<Status>>,
    cancel: Arc<AtomicBool>,
) {
    let mut checkpoint = None::<Checkpoint>;
    let mut cache = None::<Access>;
    let mut manual = false;
    let mut next_poll = Instant::now();
    let mut local_due = None::<Instant>;
    let mut failures = 0u32;
    while !platform.quit.load(Ordering::SeqCst) {
        let command = match receiver.recv_timeout(Duration::from_millis(500)) {
            Ok(command) => Some(command),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(_) => return,
        };
        let result: Result<(), Failure> = (|| {
            if let Some(command) = command {
                match command {
                    Command::Import(client) => {
                        let mut settings = shared.lock().unwrap().settings.drive.clone();
                        if settings.connected() {
                            return Err(Failure::data(
                                "Disconnect this computer before changing its Google OAuth client.",
                            ));
                        }
                        settings.client_id = client.id;
                        settings.client_secret_protected =
                            platform::protect(&client.secret).map_err(Failure::data)?;
                        settings.needs_sign_in = false;
                        save_settings(&shared, settings)?;
                        update_status(
                            &status,
                            &ctx,
                            Phase::Disconnected,
                            "OAuth client imported. Connect Google Drive to sign in.",
                        );
                    }
                    Command::Connect => {
                        let result = connect(&shared, &platform, &ctx, &status, &cancel);
                        cancel.store(false, Ordering::SeqCst);
                        cache = Some(result?);
                        checkpoint = None;
                        manual = true;
                        failures = 0;
                        next_poll = Instant::now();
                    }
                    Command::Disconnect => {
                        let mut settings = shared.lock().unwrap().settings.drive.clone();
                        settings.refresh_token_protected.clear();
                        settings.account_id.clear();
                        settings.account_label.clear();
                        settings.needs_sign_in = false;
                        save_settings(&shared, settings)?;
                        checkpoint = None;
                        cache = None;
                        manual = false;
                        local_due = None;
                        cancel.store(false, Ordering::SeqCst);
                        *status.lock().unwrap() = Status::default();
                        update_status(
                            &status,
                            &ctx,
                            Phase::Disconnected,
                            "This computer is disconnected. Local subscriptions and Google Drive snapshots were retained.",
                        );
                    }
                    Command::Sync => {
                        cancel.store(false, Ordering::SeqCst);
                        manual = true;
                        next_poll = Instant::now();
                    }
                    Command::Pause => {
                        cancel.store(false, Ordering::SeqCst);
                        manual = false;
                        local_due = None;
                        update_status(
                            &status,
                            &ctx,
                            Phase::Connected,
                            "Automatic synchronization is paused. Use Sync now to transfer changes.",
                        );
                    }
                    Command::Resolve(conflict, choice) => {
                        let cp = checkpoint.as_mut().ok_or_else(|| {
                            Failure::data("Connect Google Drive before resolving a cloud conflict.")
                        })?;
                        let mut store = shared.lock().unwrap();
                        cp.capture_local(&mut store)?;
                        let mut resolved = cp.replica.clone();
                        resolved
                            .resolve(&conflict, choice, cp.device)
                            .map_err(Failure::data)?;
                        let db = resolved.materialize(&store.db);
                        store.commit(db).map_err(Failure::data)?;
                        cp.replica = resolved;
                        cp.materialized = store.db.clone();
                        cp.save()?;
                        manual = true;
                        next_poll = Instant::now();
                    }
                }
            }
            let settings = shared.lock().unwrap().settings.drive.clone();
            if !settings.connected() {
                if settings.client_id.is_empty() {
                    update_status(
                        &status,
                        &ctx,
                        Phase::Setup,
                        "Import a Desktop OAuth client JSON once, then sign in with Google.",
                    );
                }
                return Ok(());
            }
            if settings.needs_sign_in {
                if status.lock().unwrap().phase != Phase::SignInRequired {
                    update_status(
                        &status,
                        &ctx,
                        Phase::SignInRequired,
                        "Reconnect Google Drive to renew the session. Local changes are preserved.",
                    );
                }
                return Ok(());
            }
            if checkpoint.is_none() {
                checkpoint = Some(Checkpoint::open(&shared.lock().unwrap())?);
                status.lock().unwrap().last_success = checkpoint.as_ref().unwrap().last_success;
            }
            let cp = checkpoint.as_mut().unwrap();
            if cp.capture_local(&mut shared.lock().unwrap())? {
                local_due = Some(Instant::now() + Duration::from_secs(2));
                status.lock().unwrap().pending = true;
                ctx.request_repaint();
            }
            if !settings.auto_sync && !manual {
                if status.lock().unwrap().phase != Phase::Offline {
                    update_status(
                        &status,
                        &ctx,
                        Phase::Connected,
                        "Automatic synchronization is paused. Use Sync now to transfer changes.",
                    );
                }
                return Ok(());
            }
            let due = manual
                || Instant::now() >= next_poll
                || local_due.is_some_and(|due| Instant::now() >= due && failures == 0);
            if !due {
                return Ok(());
            }
            manual = false;
            update_status(
                &status,
                &ctx,
                Phase::Syncing,
                "Synchronizing subscriptions and notes with Google Drive...",
            );
            let backend = Google::new(access(&shared, &mut cache)?).cancellable(cancel.clone());
            synchronize(&shared, cp, &backend)?;
            failures = 0;
            local_due = None;
            next_poll = Instant::now() + Duration::from_secs(60);
            {
                let mut value = status.lock().unwrap();
                value.last_success = cp.last_success;
                value.conflicts = cp.replica.conflicts();
                value.pending = false;
            }
            let count = status.lock().unwrap().conflicts.len();
            update_status(
                &status,
                &ctx,
                Phase::Connected,
                if count == 0 {
                    "Subscriptions and notes are synchronized.".into()
                } else {
                    format!(
                        "Synchronized with {count} unresolved conflict(s). Review their versions below; no version was discarded."
                    )
                },
            );
            Ok(())
        })();
        if let Err(error) = result {
            if error.access_expired {
                cache = None;
            }
            if error.sign_in {
                let mut settings = shared.lock().unwrap().settings.drive.clone();
                settings.needs_sign_in = true;
                if let Err(save_error) = save_settings(&shared, settings) {
                    update_status(&status, &ctx, Phase::Offline, save_error.message);
                }
            }
            if let Some(cp) = &checkpoint {
                status.lock().unwrap().conflicts = cp.replica.conflicts();
            }
            failures = failures.saturating_add(1);
            next_poll = Instant::now()
                + Duration::from_secs((5 * 2u64.saturating_pow(failures.min(6) - 1)).min(300));
            manual = false;
            update_status(
                &status,
                &ctx,
                if error.sign_in {
                    Phase::SignInRequired
                } else {
                    Phase::Offline
                },
                error.message,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Subscription;
    use api::CloudFile;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct Memory {
        files: Mutex<BTreeMap<String, Replica>>,
        generated: AtomicUsize,
    }
    impl Backend for Memory {
        fn list(&self) -> Result<Vec<CloudFile>, Failure> {
            self.files
                .lock()
                .unwrap()
                .iter()
                .map(|(id, state)| {
                    Ok(CloudFile {
                        id: id.clone(),
                        checksum: Some(state_hash(state)?),
                    })
                })
                .collect()
        }
        fn download(&self, id: &str) -> Result<Replica, Failure> {
            self.files
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .ok_or_else(|| Failure::retry("missing"))
        }
        fn generate_id(&self) -> Result<String, Failure> {
            Ok(format!(
                "file{}",
                self.generated.fetch_add(1, Ordering::SeqCst)
            ))
        }
        fn exists(&self, id: &str) -> Result<bool, Failure> {
            Ok(self.files.lock().unwrap().contains_key(id))
        }
        fn upload(
            &self,
            id: &str,
            _device: Uuid,
            data: &Replica,
            create: bool,
        ) -> Result<CloudFile, Failure> {
            let mut files = self.files.lock().unwrap();
            if create && files.contains_key(id) {
                return Err(Failure::retry("already created"));
            }
            files.insert(id.into(), data.clone());
            Ok(CloudFile {
                id: id.into(),
                checksum: Some(state_hash(data)?),
            })
        }
    }
    fn device(path: &std::path::Path) -> (Arc<Mutex<Store>>, Checkpoint) {
        let mut store = Store::open(path.to_owned()).unwrap();
        store.settings.drive.client_id = "sample.apps.googleusercontent.com".into();
        store.settings.drive.account_id = "account".into();
        store.save_settings().unwrap();
        let cp = Checkpoint::open(&store).unwrap();
        (Arc::new(Mutex::new(store)), cp)
    }
    fn add(store: &Arc<Mutex<Store>>, name: &str) {
        let mut store = store.lock().unwrap();
        let mut db = store.db.clone();
        let mut sub = Subscription::new("2026-11-02".parse().unwrap());
        sub.name = name.into();
        db.subscriptions.push(sub);
        store.commit(db).unwrap();
    }
    fn edit(store: &Arc<Mutex<Store>>, note: &str) {
        let mut store = store.lock().unwrap();
        let mut db = store.db.clone();
        db.subscriptions[0].notes = note.into();
        store.commit(db).unwrap();
    }
    #[test]
    fn two_devices_pull_push_and_merge_independent_offline_changes() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        let (a, mut ca) = device(left.path());
        let (b, mut cb) = device(right.path());
        let cloud = Memory::default();
        add(&a, "Cloud");
        synchronize(&a, &mut ca, &cloud).unwrap();
        synchronize(&b, &mut cb, &cloud).unwrap();
        edit(&a, "Offline note");
        add(&b, "Music");
        synchronize(&a, &mut ca, &cloud).unwrap();
        synchronize(&b, &mut cb, &cloud).unwrap();
        synchronize(&a, &mut ca, &cloud).unwrap();
        assert_eq!(a.lock().unwrap().db, b.lock().unwrap().db);
        assert_eq!(a.lock().unwrap().db.subscriptions.len(), 2);
        assert!(ca.replica.conflicts().is_empty());
        assert!(
            a.lock()
                .unwrap()
                .db
                .subscriptions
                .iter()
                .any(|s| s.notes == "Offline note")
        );
        assert_eq!(cloud.files.lock().unwrap().len(), 2);
        let store = Store::open(left.path().to_owned()).unwrap();
        assert_eq!(Checkpoint::open(&store).unwrap().device, ca.device);
    }
    #[test]
    fn concurrent_edits_are_retained_and_explicit_resolution_converges() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        let (a, mut ca) = device(left.path());
        let (b, mut cb) = device(right.path());
        let cloud = Memory::default();
        add(&a, "Cloud");
        synchronize(&a, &mut ca, &cloud).unwrap();
        synchronize(&b, &mut cb, &cloud).unwrap();
        edit(&a, "Left version");
        edit(&b, "Right version");
        synchronize(&a, &mut ca, &cloud).unwrap();
        synchronize(&b, &mut cb, &cloud).unwrap();
        synchronize(&a, &mut ca, &cloud).unwrap();
        assert_eq!(ca.replica.conflicts().len(), 1);
        assert_eq!(cb.replica.conflicts().len(), 1);
        let conflict = cb.replica.conflicts().remove(0);
        let choice = conflict
            .choices
            .iter()
            .position(|s| s.as_ref().is_some_and(|s| s.notes == "Right version"))
            .unwrap();
        cb.replica
            .resolve(&conflict, Resolution::Choose(choice), cb.device)
            .unwrap();
        {
            let mut store = b.lock().unwrap();
            let db = cb.replica.materialize(&store.db);
            store.commit(db).unwrap();
            cb.materialized = store.db.clone();
            cb.save().unwrap();
        }
        synchronize(&b, &mut cb, &cloud).unwrap();
        synchronize(&a, &mut ca, &cloud).unwrap();
        assert!(ca.replica.conflicts().is_empty());
        assert_eq!(a.lock().unwrap().db.subscriptions[0].notes, "Right version");
    }
    #[test]
    fn offline_deletion_propagates_and_stale_device_snapshots_do_not_restore_it() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        let (a, mut ca) = device(left.path());
        let (b, mut cb) = device(right.path());
        let cloud = Memory::default();
        add(&a, "Cloud");
        synchronize(&a, &mut ca, &cloud).unwrap();
        synchronize(&b, &mut cb, &cloud).unwrap();
        a.lock().unwrap().commit(Database::default()).unwrap();
        synchronize(&a, &mut ca, &cloud).unwrap();
        synchronize(&b, &mut cb, &cloud).unwrap();
        synchronize(&a, &mut ca, &cloud).unwrap();
        assert!(a.lock().unwrap().db.subscriptions.is_empty());
        assert!(b.lock().unwrap().db.subscriptions.is_empty());
    }
    #[test]
    fn an_unconfirmed_upload_retries_the_same_file_id_without_duplicate_records() {
        struct Unconfirmed<'a> {
            cloud: &'a Memory,
            fail: AtomicBool,
        }
        impl Backend for Unconfirmed<'_> {
            fn list(&self) -> Result<Vec<CloudFile>, Failure> {
                self.cloud.list()
            }
            fn download(&self, id: &str) -> Result<Replica, Failure> {
                self.cloud.download(id)
            }
            fn generate_id(&self) -> Result<String, Failure> {
                self.cloud.generate_id()
            }
            fn exists(&self, id: &str) -> Result<bool, Failure> {
                self.cloud.exists(id)
            }
            fn upload(
                &self,
                id: &str,
                device: Uuid,
                data: &Replica,
                create: bool,
            ) -> Result<CloudFile, Failure> {
                let result = self.cloud.upload(id, device, data, create)?;
                if self.fail.swap(false, Ordering::SeqCst) {
                    Err(Failure::retry("response lost"))
                } else {
                    Ok(result)
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let (store, mut cp) = device(dir.path());
        add(&store, "Cloud");
        let cloud = Memory::default();
        let flaky = Unconfirmed {
            cloud: &cloud,
            fail: AtomicBool::new(true),
        };
        assert!(synchronize(&store, &mut cp, &flaky).is_err());
        let persisted = Checkpoint::open(&store.lock().unwrap()).unwrap();
        assert_eq!(persisted.file_id, cp.file_id);
        synchronize(&store, &mut cp, &flaky).unwrap();
        assert_eq!(cloud.generated.load(Ordering::SeqCst), 1);
        assert_eq!(cloud.files.lock().unwrap().len(), 1);
        assert_eq!(store.lock().unwrap().db.subscriptions.len(), 1);
    }
    #[test]
    fn edits_during_a_download_remain_concurrent_with_unseen_remote_updates() {
        struct Racing<'a> {
            cloud: &'a Memory,
            store: Arc<Mutex<Store>>,
            edit: AtomicBool,
        }
        impl Backend for Racing<'_> {
            fn list(&self) -> Result<Vec<CloudFile>, Failure> {
                self.cloud.list()
            }
            fn download(&self, id: &str) -> Result<Replica, Failure> {
                let result = self.cloud.download(id)?;
                if self.edit.swap(false, Ordering::SeqCst) {
                    edit(&self.store, "Edited during download");
                }
                Ok(result)
            }
            fn generate_id(&self) -> Result<String, Failure> {
                self.cloud.generate_id()
            }
            fn exists(&self, id: &str) -> Result<bool, Failure> {
                self.cloud.exists(id)
            }
            fn upload(
                &self,
                id: &str,
                device: Uuid,
                data: &Replica,
                create: bool,
            ) -> Result<CloudFile, Failure> {
                self.cloud.upload(id, device, data, create)
            }
        }
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        let (a, mut ca) = device(left.path());
        let (b, mut cb) = device(right.path());
        let cloud = Memory::default();
        add(&a, "Cloud");
        synchronize(&a, &mut ca, &cloud).unwrap();
        synchronize(&b, &mut cb, &cloud).unwrap();
        edit(&a, "Remote update");
        synchronize(&a, &mut ca, &cloud).unwrap();
        let racing = Racing {
            cloud: &cloud,
            store: b.clone(),
            edit: AtomicBool::new(true),
        };
        synchronize(&b, &mut cb, &racing).unwrap();
        assert_eq!(cb.replica.conflicts().len(), 1);
        assert_eq!(
            b.lock().unwrap().db.subscriptions[0].notes,
            "Edited during download"
        );
    }
}
