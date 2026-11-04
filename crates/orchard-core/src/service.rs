use crate::{
    apple_session::AppleSession,
    catalog::{Catalog, CatalogStore, Target},
    core::{self, ConnectionMode, Inventory, Result, Settings, Snapshot},
    request_guard, retry,
};
use rusqlite::Connection;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Mutex,
    },
};

#[derive(Serialize)]
pub struct Status {
    pub settings: Settings,
    pub configured_interval_seconds: u32,
    pub active_interval_seconds: u32,
    pub busy: bool,
    pub apple_failures: u32,
    pub apple_request_after: i64,
    pub apple_request_limit_reason: Option<String>,
    pub apple_session_open: bool,
    pub server_retry_until: i64,
    pub next_due: Option<i64>,
    pub last_attempt: Option<i64>,
    pub last_error: Option<String>,
    pub db_path: String,
}

#[derive(Serialize, Debug)]
pub struct CheckResult {
    pub state: String,
    pub target: Target,
    pub snapshot: Option<Snapshot>,
    pub last_known: Option<Snapshot>,
    pub newly_available: Vec<Inventory>,
    pub retry_at_ms: Option<i64>,
    pub message: Option<String>,
}

pub struct Service {
    db: Mutex<Connection>,
    pub session: AppleSession,
    pub catalog: CatalogStore,
    pub db_path: PathBuf,
    busy: AtomicBool,
    transient_retry: AtomicI64,
}
struct BusyGuard<'a>(&'a AtomicBool);
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Service {
    pub fn new(path: &Path) -> Result<Self> {
        let directory = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let db = Connection::open(path).map_err(|e| e.to_string())?;
        db.busy_timeout(std::time::Duration::from_secs(15))
            .map_err(|e| e.to_string())?;
        core::initialize(&db)?;
        request_guard::initialize(&db)?;
        let settings = core::load_settings(&db)?;
        let (_, _, server_until) = retry::saved_state(&db, &settings.url)?;
        request_guard::hold_until(&db, server_until, "server_wait")?;
        let session = AppleSession::default();
        session.configure_diagnostics(directory);
        session.restore_retry_until(server_until);
        Ok(Self {
            db: Mutex::new(db),
            session,
            catalog: CatalogStore::new(directory),
            db_path: path.into(),
            busy: AtomicBool::new(false),
            transient_retry: AtomicI64::new(0),
        })
    }
    pub fn db(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.db.lock().map_err(|_| "Database lock failed.".into())
    }
    fn acquire(&self) -> Result<BusyGuard<'_>> {
        self.busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "An inventory check is already running.".to_string())?;
        Ok(BusyGuard(&self.busy))
    }
    pub fn defer_error(&self) {
        self.transient_retry
            .store(core::now_ms() + 60000, Ordering::SeqCst);
    }
    pub fn status(&self) -> Result<Status> {
        let db = self.db()?;
        let settings = core::load_settings(&db)?;
        let now = core::now_ms();
        let last_attempt = core::last_attempt(&db, &settings.url)?;
        let last_error = core::last_error(&db, &settings.url)?;
        let gate = request_guard::read(&db)?;
        let (failures, saved_due, _) = retry::saved_state(&db, &settings.url)?;
        let due = settings
            .next_check(last_attempt, now)
            .max(saved_due.unwrap_or(0))
            .max(gate.not_before_ms)
            .max(self.transient_retry.load(Ordering::SeqCst));
        let configured = settings.active_interval(now);
        Ok(Status {
            configured_interval_seconds: configured,
            active_interval_seconds: request_guard::effective_interval(configured),
            busy: self.busy.load(Ordering::SeqCst),
            apple_failures: failures,
            apple_request_after: gate.not_before_ms,
            apple_request_limit_reason: (gate.not_before_ms > now).then_some(gate.reason.clone()),
            apple_session_open: self.session.is_manual_open(),
            server_retry_until: if gate.reason == "server_wait" {
                gate.not_before_ms
            } else {
                self.session.retry_until()
            },
            next_due: settings.enabled.then_some(due),
            settings,
            last_attempt,
            last_error,
            db_path: self.db_path.to_string_lossy().into(),
        })
    }
    pub fn save_settings(&self, settings: &Settings) -> Result<Status> {
        settings.validate()?;
        if !settings.target.zip_code.is_empty() {
            self.catalog.load().resolve(&settings.target)?;
        }
        core::store_settings(&*self.db()?, settings)?;
        self.transient_retry.store(0, Ordering::SeqCst);
        self.status()
    }
    pub fn catalog(&self) -> Catalog {
        self.catalog.load()
    }
    pub fn check(
        &self,
        target: Option<Target>,
        mode: Option<ConnectionMode>,
        interval: Option<u32>,
        reconnect: bool,
    ) -> Result<CheckResult> {
        if interval.is_some_and(|seconds| !(1..=86400).contains(&seconds)) {
            return Err("Check interval must be between 1 second and 24 hours.".into());
        }
        let _busy = self.acquire()?;
        let settings = core::load_settings(&*self.db()?)?;
        let target = target.unwrap_or_else(|| settings.target.clone());
        let key = target.key();
        let resolved = self.catalog.load().resolve(&target)?;
        let last_known = core::latest_snapshot(&*self.db()?, &key)?;
        let _process_lock = match self.process_lock()? {
            Some(lock) => lock,
            None => return Ok(CheckResult {
                state: "deferred".into(),
                target,
                snapshot: None,
                last_known,
                newly_available: Vec::new(),
                retry_at_ms: Some(core::now_ms() + 60000),
                message: Some(
                    "Another Orchard client is checking inventory. Retry after the supplied time."
                        .into(),
                ),
            }),
        };
        {
            let db = self.db()?;
            if let Err(gate) = request_guard::claim(&db, core::now_ms())? {
                return Ok(CheckResult {
                    state: "deferred".into(),
                    target,
                    snapshot: None,
                    last_known,
                    newly_available: Vec::new(),
                    retry_at_ms: Some(gate.not_before_ms),
                    message: Some(gate_message(&gate)),
                });
            }
        }
        let mode = mode.unwrap_or(settings.connection);
        let fetched = if reconnect {
            self.session.reconnect_and_fetch(&resolved, mode)
        } else {
            self.session.fetch(&resolved, mode)
        };
        let observed_at = core::now_ms();
        let (error, source_at, inventory) = match fetched.and_then(|(at, mut rows)| {
            validate_inventory(&resolved, &mut rows)?;
            Ok((at, rows))
        }) {
            Ok((at, rows)) => (None, at, rows),
            Err(error) => (Some(error), None, Vec::new()),
        };
        let mut snapshot = Snapshot {
            id: 0,
            source: core::Source::Apple,
            url: key.clone(),
            observed_at,
            source_at,
            error,
            next_check_at: None,
            inventory,
        };
        let changes = if snapshot.error.is_none() {
            core::newly_available(last_known.as_ref(), &snapshot)
        } else {
            Vec::new()
        };
        let mut db = self.db()?;
        let current = core::load_settings(&db)?;
        let regular_interval = interval.unwrap_or_else(|| current.active_interval(observed_at));
        if !(1..=86400).contains(&regular_interval) {
            return Err("Check interval must be between 1 second and 24 hours.".into());
        }
        let mut due = if current.url == key && interval.is_none() {
            current.next_check(Some(observed_at), observed_at)
        } else {
            observed_at + request_guard::effective_interval(regular_interval) as i64 * 1000
        };
        if let Some(error) = snapshot
            .error
            .as_deref()
            .filter(|error| retry::is_temporary_apple_error(error))
        {
            let failures = retry::saved_state(&db, &key)?.0.saturating_add(1);
            let delay = if request_guard::is_blocked(error) {
                request_guard::block_delay_ms(failures)
            } else {
                retry::failure_delay_ms(failures, regular_interval)
            };
            due = due.max(observed_at.saturating_add(delay));
            if request_guard::is_blocked(error) {
                request_guard::hold_until(&db, observed_at.saturating_add(delay), "cooldown")?;
            }
        }
        request_guard::hold_until(
            &db,
            observed_at + request_guard::MIN_INTERVAL_SECONDS as i64 * 1000,
            "pacing",
        )?;
        request_guard::hold_until(&db, self.session.retry_until(), "server_wait")?;
        due = due.max(request_guard::read(&db)?.not_before_ms);
        snapshot.next_check_at = Some(due);
        snapshot.id = core::insert_snapshot(&mut db, &snapshot)?;
        self.session.snapshot_saved(snapshot.id, due);
        self.transient_retry.store(0, Ordering::SeqCst);
        Ok(CheckResult {
            state: if snapshot.error.is_some() {
                "failed"
            } else {
                "checked"
            }
            .into(),
            message: snapshot.error.clone(),
            target,
            snapshot: Some(snapshot),
            last_known,
            newly_available: changes,
            retry_at_ms: Some(due),
        })
    }
    pub fn open_session(&self) -> Result<()> {
        let _busy = self.acquire()?;
        let _process_lock = self
            .process_lock()?
            .ok_or("Another Orchard client is checking inventory.")?;
        let settings = core::load_settings(&*self.db()?)?;
        let resolved = self.catalog.load().resolve(&settings.target)?;
        if let Err(gate) = request_guard::claim(&*self.db()?, core::now_ms())? {
            return Err(gate_message(&gate));
        }
        let result = self.session.open(&resolved);
        let db = self.db()?;
        if result
            .as_ref()
            .err()
            .is_some_and(|e| request_guard::is_blocked(e))
        {
            request_guard::hold_until(
                &db,
                core::now_ms() + request_guard::BLOCK_COOLDOWN_MS,
                "cooldown",
            )?;
        }
        request_guard::hold_until(&db, self.session.retry_until(), "server_wait")?;
        result
    }
    fn process_lock(&self) -> Result<Option<std::fs::File>> {
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.db_path.with_extension("request.lock"))
            .map_err(|e| e.to_string())?;
        match fs2::FileExt::try_lock_exclusive(&lock) {
            Ok(()) => Ok(Some(lock)),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }
}
fn validate_inventory(
    target: &crate::catalog::ResolvedTarget,
    rows: &mut Vec<Inventory>,
) -> Result<()> {
    let expected: std::collections::BTreeSet<_> = target
        .variants
        .iter()
        .map(|v| v.part_number.as_str())
        .collect();
    rows.retain(|r| expected.contains(r.part_number.as_str()));
    if rows.is_empty() {
        return Err("Apple inventory returned none of the requested configurations.".into());
    }
    let stores: std::collections::BTreeSet<_> = rows.iter().map(|r| r.store_id.clone()).collect();
    for store in stores {
        if store.is_empty() {
            return Err("Apple inventory returned an unidentified store.".into());
        }
        let actual: std::collections::BTreeSet<_> = rows
            .iter()
            .filter(|r| r.store_id == store)
            .map(|r| r.part_number.as_str())
            .collect();
        if !expected.is_subset(&actual) {
            return Err("Apple inventory omitted requested configurations; current availability is unknown.".into());
        }
    }
    if rows
        .iter()
        .any(|r| !["available", "unavailable", "ineligible"].contains(&r.status.as_str()))
    {
        return Err("Apple inventory returned an unknown pickup status.".into());
    }
    if target.category == "mac" {
        for row in rows {
            if let Some(variant) = target
                .variants
                .iter()
                .find(|v| v.part_number == row.part_number)
            {
                row.product = variant.title.clone();
            }
        }
    }
    Ok(())
}
impl Drop for Service {
    fn drop(&mut self) {
        self.session.shutdown();
    }
}
pub fn gate_message(gate: &request_guard::Gate) -> String {
    let at = chrono::DateTime::from_timestamp_millis(gate.not_before_ms)
        .map(|at| at.to_rfc3339())
        .unwrap_or_else(|| gate.not_before_ms.to_string());
    if gate.reason == "cooldown" {
        format!("Apple checks are cooling down after a blocked request. Next allowed check: {at}.")
    } else if gate.reason == "server_wait" {
        format!("Apple supplied Retry-After. Next allowed check: {at}.")
    } else {
        format!("Apple checks are paced to at least one minute. Next allowed check: {at}.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temporary() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir()
            .join(format!(
                "orchard-service-{}-{}-{}",
                std::process::id(),
                core::now_ms(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ))
            .join("inventory.sqlite3")
    }
    #[test]
    fn migration_preserves_history_settings_and_cooldown_for_all_clients() {
        let path = temporary();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let db = Connection::open(&path).unwrap();
        core::initialize(&db).unwrap();
        let legacy = serde_json::json!({"source":"checker","url":"https://iphonechecker.herokuapp.com/q/94025/iDuo/unlocked","interval_seconds":600,"enabled":true,"notify_available":true,"windows":[{"enabled":true,"start_minute":480,"end_minute":710,"interval_seconds":120}]});
        db.execute("UPDATE settings SET value=?1", [legacy.to_string()])
            .unwrap();
        let now = core::now_ms();
        db.execute("INSERT INTO snapshots(id,observed_at,url,source,error,next_check_at) VALUES(42,?1,?2,'apple','HTTP 541',?3)",rusqlite::params![now,"https://iphonechecker.herokuapp.com/q/94025/iDuo/unlocked",now+900000]).unwrap();
        drop(db);
        let first = Service::new(&path).unwrap();
        let status = first.status().unwrap();
        assert_eq!(status.settings.source, core::Source::Apple);
        assert_eq!(status.settings.url, "apple://us/94025/iphone-duo/unlocked");
        assert_eq!(status.settings.windows[0].interval_seconds, 120);
        assert!(status.settings.enabled && status.settings.notify_available);
        let second = Service::new(&path).unwrap();
        let other = Target {
            device_id: "mac-mini".into(),
            zip_code: "94025".into(),
            part_number: String::new(),
            carrier: "unlocked".into(),
        };
        let blocked = second
            .check(Some(other), Some(ConnectionMode::Browser), None, false)
            .unwrap();
        assert_eq!(blocked.state, "deferred");
        assert!(blocked.snapshot.is_none());
        assert!(blocked.retry_at_ms.unwrap() >= now + 900000);
        assert_eq!(
            first.status().unwrap().settings.target.device_id,
            "iphone-duo"
        );
        assert_eq!(
            second
                .db()
                .unwrap()
                .query_row("SELECT count(*) FROM snapshots", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            core::snapshot_by_id(&second.db().unwrap(), 42)
                .unwrap()
                .observed_at,
            now
        );
        drop(first);
        drop(second);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn process_lock_defers_another_client_without_inventing_a_check() {
        let path = temporary();
        let first = Service::new(&path).unwrap();
        let second = Service::new(&path).unwrap();
        let lock = first.process_lock().unwrap().unwrap();
        let target = Target::from_key(core::DEFAULT_URL).unwrap();
        let result = second.check(Some(target), None, None, false).unwrap();
        assert_eq!(result.state, "deferred");
        assert!(result.snapshot.is_none());
        assert_eq!(
            second
                .db()
                .unwrap()
                .query_row("SELECT count(*) FROM snapshots", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(lock);
        drop(first);
        drop(second);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn incomplete_inventory_is_unknown_and_mac_skus_keep_distinct_names() {
        let target = crate::catalog::builtin()
            .resolve(&Target {
                device_id: "mac-mini".into(),
                zip_code: "94025".into(),
                part_number: String::new(),
                carrier: "unlocked".into(),
            })
            .unwrap();
        let mut rows: Vec<_> = target
            .variants
            .iter()
            .map(|v| Inventory {
                store: "Example".into(),
                store_id: "R001".into(),
                city: "Example".into(),
                distance: "1 mi".into(),
                product: "Mac mini".into(),
                part_number: v.part_number.clone(),
                status: "available".into(),
                pickup_quote: "Available Today".into(),
            })
            .collect();
        validate_inventory(&target, &mut rows).unwrap();
        assert_ne!(rows[0].product, rows[1].product);
        rows.pop();
        assert!(validate_inventory(&target, &mut rows).is_err());
    }
}
