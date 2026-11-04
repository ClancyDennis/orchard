use crate::core::{now_ms, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

const MAX_BYTES: u64 = 4 * 1024 * 1024;
const ROTATIONS: usize = 3;
const RECENT_LIMIT: usize = 40;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CookieMetadata {
    pub name: String,
    pub domain: String,
    pub expires_at_ms: Option<i64>,
    pub session: bool,
    pub secure: bool,
    pub http_only: bool,
}
pub fn cookie_metadata(data: &Value) -> Vec<CookieMetadata> {
    data.get("cookies")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(200)
        .filter_map(|cookie| {
            let domain = cookie.get("domain")?.as_str()?;
            let host = domain.trim_start_matches('.');
            if host != "apple.com" && !host.ends_with(".apple.com") {
                return None;
            }
            let expiry = cookie.get("expires").and_then(Value::as_f64);
            Some(CookieMetadata {
                name: cookie.get("name")?.as_str()?.chars().take(100).collect(),
                domain: domain.chars().take(100).collect(),
                expires_at_ms: expiry
                    .filter(|expiry| {
                        expiry.is_finite() && *expiry > 0.0 && *expiry < 253_402_300_800.0
                    })
                    .map(|expiry| (expiry * 1000.0) as i64),
                session: cookie
                    .get("session")
                    .and_then(Value::as_bool)
                    .unwrap_or(expiry.is_none_or(|expiry| expiry <= 0.0)),
                secure: cookie
                    .get("secure")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                http_only: cookie
                    .get("httpOnly")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PageMetadata {
    pub in_storefront: bool,
    pub ready: Option<String>,
    pub visibility: Option<String>,
    pub bootstrap: bool,
}
pub fn page_metadata(value: &Value) -> PageMetadata {
    PageMetadata {
        in_storefront: value["inStorefront"].as_bool().unwrap_or(false),
        ready: value["ready"]
            .as_str()
            .filter(|value| ["loading", "interactive", "complete"].contains(value))
            .map(str::to_string),
        visibility: value["visibility"]
            .as_str()
            .filter(|value| ["visible", "hidden"].contains(value))
            .map(str::to_string),
        bootstrap: value["bootstrap"].as_bool().unwrap_or(false),
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ResponseMetadata {
    pub endpoint: Option<String>,
    pub transport: Option<String>,
    pub http_status: Option<u16>,
    pub content_type: Option<String>,
    pub body_kind: Option<String>,
    pub body_bytes: Option<u64>,
    pub retry_until_ms: Option<i64>,
    pub server_date_ms: Option<i64>,
}
pub fn response_metadata(value: &Value) -> ResponseMetadata {
    let response = &value["response"];
    ResponseMetadata {
        endpoint: None,
        transport: None,
        http_status: response["status"]
            .as_u64()
            .filter(|status| (100..=599).contains(status))
            .map(|status| status as u16),
        content_type: response["contentType"]
            .as_str()
            .filter(|value| {
                ["application/json", "text/html", "text/plain", "other"].contains(value)
            })
            .map(str::to_string),
        body_kind: response["bodyKind"]
            .as_str()
            .filter(|value| {
                [
                    "json",
                    "not_found",
                    "maintenance",
                    "access_denied",
                    "verification",
                    "unexpected_html",
                    "empty",
                    "other",
                ]
                .contains(value)
            })
            .map(str::to_string),
        body_bytes: response["bytes"].as_u64(),
        retry_until_ms: None,
        server_date_ms: response["date"]
            .as_str()
            .and_then(|date| chrono::DateTime::parse_from_rfc2822(date).ok())
            .map(|date| date.timestamp_millis()),
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Sample {
    pub page: PageMetadata,
    pub cookies: Vec<CookieMetadata>,
    pub cookies_available: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConnectionEvent {
    pub at_ms: i64,
    pub event: String,
    pub check_id: Option<String>,
    pub snapshot_id: Option<i64>,
    pub next_check_at_ms: Option<i64>,
    pub profile_id: Option<String>,
    pub profile_created_at_ms: Option<i64>,
    pub browser_started_at_ms: Option<i64>,
    pub page_loaded_at_ms: Option<i64>,
    pub elapsed_ms: Option<u64>,
    pub operation: Option<String>,
    pub success: Option<bool>,
    pub error_kind: Option<String>,
    pub inventory_rows: Option<usize>,
    pub response: ResponseMetadata,
    pub before: Option<Sample>,
    pub after: Option<Sample>,
    pub expired_since_previous_sample: Vec<String>,
}
#[derive(Clone, Serialize)]
pub struct ConnectionReport {
    pub latest: Option<ConnectionEvent>,
    pub recent: Vec<ConnectionEvent>,
    pub last_success_at_ms: Option<i64>,
    pub logging_available: bool,
}
#[derive(Default)]
struct LogState {
    path: Option<PathBuf>,
    recent: VecDeque<ConnectionEvent>,
    last_success_at_ms: Option<i64>,
    last_success_written_at_ms: i64,
    last_written_check_id: Option<String>,
    logging_available: bool,
}
#[derive(Default)]
pub struct ConnectionLog {
    state: Mutex<LogState>,
}
fn rotated(path: &Path, number: usize) -> PathBuf {
    path.with_extension(format!("jsonl.{number}"))
}
fn append(path: &Path, event: &ConnectionEvent) -> Result<()> {
    if fs::metadata(path).map(|meta| meta.len()).unwrap_or(0) >= MAX_BYTES {
        for index in (1..=ROTATIONS).rev() {
            let from = if index == 1 {
                path.to_path_buf()
            } else {
                rotated(path, index - 1)
            };
            let to = rotated(path, index);
            if to.exists() {
                fs::remove_file(&to).map_err(|e| e.to_string())?;
            }
            if from.exists() {
                fs::rename(from, to).map_err(|e| e.to_string())?;
            }
        }
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| e.to_string())?;
    let line = serde_json::to_string(event).map_err(|e| e.to_string())?;
    writeln!(file, "{line}").map_err(|e| e.to_string())
}
impl ConnectionLog {
    pub fn configure(&self, directory: &Path) {
        if let Ok(mut state) = self.state.lock() {
            if state.path.is_some() {
                return;
            }
            let path = directory.join("apple-connection.jsonl");
            for index in (0..=ROTATIONS).rev() {
                let file = if index == 0 {
                    path.clone()
                } else {
                    rotated(&path, index)
                };
                if let Ok(text) = fs::read_to_string(file) {
                    for line in text.lines() {
                        if let Ok(event) = serde_json::from_str::<ConnectionEvent>(line) {
                            if event.success == Some(true) && event.event == "inventory_check" {
                                state.last_success_at_ms = Some(event.at_ms);
                            }
                            if event.event == "snapshot_saved" {
                                if let Some(check) = state.recent.iter_mut().rev().find(|check| {
                                    check.event == "inventory_check"
                                        && check.check_id == event.check_id
                                }) {
                                    check.snapshot_id = event.snapshot_id;
                                    check.next_check_at_ms = event.next_check_at_ms;
                                }
                            }
                            state.recent.push_back(event);
                            if state.recent.len() > RECENT_LIMIT {
                                state.recent.pop_front();
                            }
                        }
                    }
                }
            }
            state.path = Some(path);
            state.logging_available = true;
        }
    }
    pub fn record(&self, mut event: ConnectionEvent) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if event.at_ms == 0 {
            event.at_ms = now_ms();
        }
        if let Some(current) = &event.before {
            if current.cookies_available {
                if let Some(previous) = state.recent.iter().rev().find(|previous| {
                    previous.profile_id == event.profile_id
                        && previous
                            .after
                            .as_ref()
                            .is_some_and(|sample| sample.cookies_available)
                }) {
                    if let Some(sample) = &previous.after {
                        event.expired_since_previous_sample = sample
                            .cookies
                            .iter()
                            .filter(|cookie| {
                                cookie
                                    .expires_at_ms
                                    .is_some_and(|expires| expires <= event.at_ms)
                                    && !current.cookies.iter().any(|current| {
                                        current.name == cookie.name
                                            && current.domain == cookie.domain
                                    })
                            })
                            .map(|cookie| cookie.name.clone())
                            .collect();
                    }
                }
            }
        }
        if event.event == "inventory_check" && event.success == Some(true) {
            state.last_success_at_ms = Some(event.at_ms);
        }
        let should_write = !(event.event == "inventory_check"
            && event.success == Some(true)
            && event.operation.as_deref() == Some("check")
            && event.at_ms - state.last_success_written_at_ms < 60_000);
        if should_write {
            if let Some(path) = &state.path {
                state.logging_available = append(path, &event).is_ok();
            }
            if event.event == "inventory_check" {
                state.last_written_check_id = event.check_id.clone();
                if event.success == Some(true) {
                    state.last_success_written_at_ms = event.at_ms;
                }
            }
        }
        state.recent.push_back(event);
        if state.recent.len() > RECENT_LIMIT {
            state.recent.pop_front();
        }
    }
    pub fn snapshot_saved(&self, snapshot_id: i64, next_check_at: i64) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let Some(event) = state
            .recent
            .iter_mut()
            .rev()
            .find(|event| event.event == "inventory_check")
        else {
            return;
        };
        event.snapshot_id = Some(snapshot_id);
        event.next_check_at_ms = Some(next_check_at);
        let check_id = event.check_id.clone();
        if check_id == state.last_written_check_id {
            let record = ConnectionEvent {
                at_ms: now_ms(),
                event: "snapshot_saved".into(),
                check_id,
                snapshot_id: Some(snapshot_id),
                next_check_at_ms: Some(next_check_at),
                ..Default::default()
            };
            if let Some(path) = &state.path {
                state.logging_available = append(path, &record).is_ok();
            }
        }
    }
    pub fn report(&self) -> ConnectionReport {
        let Ok(state) = self.state.lock() else {
            return ConnectionReport {
                latest: None,
                recent: vec![],
                last_success_at_ms: None,
                logging_available: false,
            };
        };
        ConnectionReport {
            latest: state
                .recent
                .iter()
                .rev()
                .find(|event| event.event == "inventory_check")
                .cloned(),
            recent: state.recent.iter().cloned().collect(),
            last_success_at_ms: state.last_success_at_ms,
            logging_available: state.logging_available,
        }
    }
    pub fn export(&self) -> Result<String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "Connection log lock failed.")?;
        let mut records = vec![];
        if let Some(path) = &state.path {
            for index in (0..=ROTATIONS).rev() {
                let file = if index == 0 {
                    path.clone()
                } else {
                    rotated(path, index)
                };
                if let Ok(text) = fs::read_to_string(file) {
                    for line in text.lines() {
                        if let Ok(event) = serde_json::from_str::<ConnectionEvent>(line) {
                            records.push(event);
                        }
                    }
                }
            }
        }
        if records.is_empty() {
            records.extend(state.recent.iter().cloned());
        }
        serde_json::to_string_pretty(&records).map_err(|e| e.to_string())
    }
}

pub fn error_kind(error: &str) -> &'static str {
    if error.contains("Retry-After") {
        "server_wait"
    } else if error.contains("HTTP") {
        "http_response"
    } else if error.contains("disconnected") || error.contains("session closed") {
        "connection"
    } else if error.contains("storefront")
        || error.contains("initialize")
        || error.contains("start")
    {
        "storefront_initialization"
    } else if error.contains("timed out") {
        "timeout"
    } else if error.contains("response") || error.contains("inventory") {
        "response_parsing"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "orchard-log-test-{}-{}-{}",
                std::process::id(),
                now_ms(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn reconnect_results_are_written_even_when_routine_successes_are_sampled() {
        let directory = TestDirectory::new();
        let log = ConnectionLog::default();
        log.configure(&directory.0);
        let record = |at, op: &str, id: &str| ConnectionEvent {
            at_ms: at,
            event: "inventory_check".into(),
            operation: Some(op.into()),
            check_id: Some(id.into()),
            success: Some(true),
            ..Default::default()
        };
        log.record(record(100_000, "check", "first"));
        log.snapshot_saved(1, 200_000);
        log.record(record(110_000, "check", "quiet"));
        log.record(record(120_000, "reconnect", "reconnected"));
        log.snapshot_saved(2, 300_000);
        log.record(record(125_000, "session_renewal", "renewed"));
        log.snapshot_saved(3, 400_000);
        let records: Vec<ConnectionEvent> = serde_json::from_str(&log.export().unwrap()).unwrap();
        assert!(!records
            .iter()
            .any(|event| event.check_id.as_deref() == Some("quiet")));
        assert!(records
            .iter()
            .any(|event| event.operation.as_deref() == Some("reconnect")
                && event.success == Some(true)));
        assert!(records
            .iter()
            .any(|event| event.event == "snapshot_saved" && event.snapshot_id == Some(2)));
        assert!(records.iter().any(
            |event| event.operation.as_deref() == Some("session_renewal")
                && event.success == Some(true)
        ));
        assert_eq!(log.report().latest.unwrap().snapshot_id, Some(3));
        let restarted = ConnectionLog::default();
        restarted.configure(&directory.0);
        assert_eq!(restarted.report().latest.unwrap().snapshot_id, Some(3));
    }
    #[test]
    fn connection_log_rotates_and_exports_only_valid_structured_records() {
        let directory = TestDirectory::new();
        let path = directory.0.join("apple-connection.jsonl");
        let record = ConnectionEvent {
            at_ms: 1,
            event: "inventory_check".into(),
            ..Default::default()
        };
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_BYTES).unwrap();
        drop(file);
        append(&path, &record).unwrap();
        assert!(rotated(&path, 1).exists());
        assert!(fs::metadata(&path).unwrap().len() < MAX_BYTES);
        let log = ConnectionLog::default();
        log.configure(&directory.0);
        let records: Vec<ConnectionEvent> = serde_json::from_str(&log.export().unwrap()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].event, "inventory_check");
    }
    #[test]
    fn metadata_whitelist_excludes_cookie_values_and_arbitrary_response_fields() {
        let cookies = cookie_metadata(
            &json!({"cookies":[{"name":"as_dc","domain":".apple.com","expires":123.0,"session":false,"secure":true,"httpOnly":true,"value":"COOKIE_SECRET"},{"name":"other","domain":"example.com","value":"ANOTHER_SECRET"}]}),
        );
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].expires_at_ms, Some(123000));
        let response = response_metadata(
            &json!({"response":{"status":541,"contentType":"text/html","bodyKind":"not_found","bytes":123,"Set-Cookie":"HEADER_SECRET"},"body":"BODY_SECRET"}),
        );
        let serialized = serde_json::to_string(&(cookies, response)).unwrap();
        assert!(
            !serialized.contains("SECRET")
                && !serialized.contains("value")
                && !serialized.contains("Set-Cookie")
        );
    }
    #[test]
    fn expired_cookie_evidence_is_limited_to_the_same_profile() {
        let log = ConnectionLog::default();
        let cookie = CookieMetadata {
            name: "expiry".into(),
            domain: ".apple.com".into(),
            expires_at_ms: Some(2000),
            session: false,
            secure: true,
            http_only: true,
        };
        log.record(ConnectionEvent {
            at_ms: 1000,
            event: "inventory_check".into(),
            profile_id: Some("same".into()),
            after: Some(Sample {
                cookies: vec![cookie],
                cookies_available: true,
                ..Default::default()
            }),
            ..Default::default()
        });
        log.record(ConnectionEvent {
            at_ms: 3000,
            event: "inventory_check".into(),
            profile_id: Some("different".into()),
            before: Some(Sample {
                cookies_available: true,
                ..Default::default()
            }),
            ..Default::default()
        });
        assert!(log
            .report()
            .latest
            .unwrap()
            .expired_since_previous_sample
            .is_empty());
        log.record(ConnectionEvent {
            at_ms: 3000,
            event: "inventory_check".into(),
            profile_id: Some("same".into()),
            before: Some(Sample {
                cookies_available: true,
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(
            log.report().latest.unwrap().expired_since_previous_sample,
            vec!["expiry"]
        );
    }
}
