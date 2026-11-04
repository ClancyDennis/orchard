use crate::connection_diagnostics::{
    self as diagnostics, ConnectionEvent, ConnectionLog, ConnectionReport, ResponseMetadata, Sample,
};
use crate::core::{
    apple_fulfillment_query, apple_query, now_ms, parse_apple_inventory, Inventory, Result,
};
use serde_json::{json, Value};
use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tungstenite::{connect, stream::MaybeTlsStream, Message, WebSocket};

const TIMEOUT: Duration = Duration::from_secs(40);
const MAX_BODY: usize = 4 * 1024 * 1024;
const RENEWAL_MARGIN_MS: i64 = 5_000;
type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

#[derive(Default)]
pub struct AppleSession {
    session: Mutex<Option<BrowserSession>>,
    pid: AtomicU32,
    profile: Mutex<Option<Arc<BrowserProfile>>>,
    manual_open: AtomicBool,
    server_retry_until: AtomicI64,
    diagnostics: ConnectionLog,
}

fn session_renewal_due(valid_until: Option<i64>, now: i64, manual_open: bool) -> bool {
    !manual_open
        && valid_until.is_some_and(|expiry| expiry <= now.saturating_add(RENEWAL_MARGIN_MS))
}
fn session_valid_until(sample: &Sample) -> Option<i64> {
    sample
        .cookies
        .iter()
        .filter(|cookie| ["shld_bt_m", "shld_bt_ck"].contains(&cookie.name.as_str()))
        .filter_map(|cookie| cookie.expires_at_ms)
        .min()
}

#[derive(Default)]
struct StorefrontRequest {
    request_id: String,
    loader_id: String,
    response: ResponseMetadata,
    completed: bool,
    failed: bool,
}
impl StorefrontRequest {
    fn relevant_url(value: &Value) -> bool {
        value.as_str().is_some_and(|url| {
            reqwest::Url::parse(url).is_ok_and(|url| {
                url.origin().ascii_serialization() == "https://www.apple.com"
                    && ["/shop/fulfillment-messages", "/shop/retail/pickup-message"]
                        .contains(&url.path())
            })
        })
    }
    fn observe(current: &mut Option<Self>, message: &Value) {
        let params = &message["params"];
        match message["method"].as_str() {
            Some("Network.requestWillBeSent") if Self::relevant_url(&params["request"]["url"]) => {
                *current = Some(Self {
                    request_id: params["requestId"].as_str().unwrap_or_default().into(),
                    loader_id: params["loaderId"].as_str().unwrap_or_default().into(),
                    ..Default::default()
                });
            }
            Some("Network.responseReceived") => {
                let response = &params["response"];
                if !Self::relevant_url(&response["url"]) {
                    return;
                }
                // Retain only public response metadata, never full headers or bodies.
                let header = |name: &str| {
                    response["headers"].as_object().and_then(|headers| {
                        headers
                            .iter()
                            .find(|(key, _)| key.eq_ignore_ascii_case(name))
                            .and_then(|(_, value)| value.as_str())
                    })
                };
                let content_type = header("content-type")
                    .unwrap_or_default()
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim();
                let mut metadata = diagnostics::response_metadata(&json!({"response": {
                    "status": response["status"].as_f64().map(|status| status as u16),
                    "contentType": content_type,
                    "date": header("date"),
                }}));
                let retry_until = retry_after_at(header("retry-after"), now_ms());
                metadata.retry_until_ms = (retry_until > 0).then_some(retry_until);
                *current = Some(Self {
                    request_id: params["requestId"].as_str().unwrap_or_default().into(),
                    loader_id: params["loaderId"].as_str().unwrap_or_default().into(),
                    response: metadata,
                    ..Default::default()
                });
            }
            Some("Network.loadingFinished" | "Network.loadingFailed") => {
                if let Some(request) = current.as_mut().filter(|request| {
                    params["requestId"].as_str() == Some(request.request_id.as_str())
                }) {
                    request.completed = true;
                    request.failed = message["method"] == "Network.loadingFailed";
                }
            }
            _ => {}
        }
    }
    fn outcome(&self) -> Option<Result<()>> {
        if self
            .response
            .http_status
            .is_some_and(|status| status >= 400)
        {
            let mut error = format!("Apple storefront's initial availability request returned HTTP {}. The next scheduled check will retry.", self.response.http_status.unwrap());
            if self
                .response
                .retry_until_ms
                .is_some_and(|until| until > now_ms())
            {
                error.push_str(" Apple supplied Retry-After; wait until the next scheduled check.");
            }
            Some(Err(error))
        } else if self.failed {
            Some(Err("Apple storefront's initial availability request failed. The next scheduled check will retry.".into()))
        } else if self.completed && self.response.http_status == Some(200) {
            Some(Ok(()))
        } else {
            None
        }
    }
}

fn storefront(target: &crate::catalog::ResolvedTarget) -> Result<String> {
    apple_query(target)?;
    Ok(target.storefront_url.clone())
}
fn browser_executable() -> Result<PathBuf> {
    [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome", "/usr/bin/chromium",
    ].iter().map(PathBuf::from).find(|path| path.is_file())
        .ok_or_else(|| "Apple checks require Google Chrome or Microsoft Edge installed on this Mac. Your saved history is still available.".into())
}
fn websocket_is_local(url: &str, port: u16) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| {
        u.scheme() == "ws"
            && u.host_str() == Some("127.0.0.1")
            && u.port() == Some(port)
            && u.username().is_empty()
            && u.password().is_none()
            && u.path().starts_with("/devtools/page/")
    })
}
fn inventory_reply(value: Value) -> Result<(Option<i64>, Vec<Inventory>)> {
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(error.to_string());
    }
    let body = value
        .get("body")
        .and_then(Value::as_str)
        .ok_or("Apple browser session returned no inventory response.")?;
    if body.len() > MAX_BODY {
        return Err("Apple inventory response was too large.".into());
    }
    parse_apple_inventory(body)
}

struct BrowserProfile {
    path: PathBuf,
    created_at_ms: i64,
}
impl BrowserProfile {
    fn create() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "orchard-apple-session-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).map_err(|e| e.to_string())?;
        Ok(Self {
            path,
            created_at_ms: now_ms(),
        })
    }
}
impl Drop for BrowserProfile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn retry_after_at(header: Option<&str>, now: i64) -> i64 {
    let Some(header) = header else {
        return 0;
    };
    if let Ok(seconds) = header.trim().parse::<u64>() {
        return now.saturating_add(seconds.saturating_mul(1000).min(i64::MAX as u64) as i64);
    }
    chrono::DateTime::parse_from_rfc2822(header)
        .ok()
        .map(|date| date.timestamp_millis())
        .filter(|at| *at > now)
        .unwrap_or(0)
}

struct BrowserSession {
    child: Child,
    _profile: Arc<BrowserProfile>,
    socket: Option<Socket>,
    sequence: u64,
    target_id: String,
    page: String,
    window_id: Option<i64>,
    minimized: bool,
    server_retry_until: i64,
    websocket_url: String,
    started_at_ms: i64,
    page_loaded_at_ms: Option<i64>,
    last_response: ResponseMetadata,
    session_valid_until_ms: Option<i64>,
    initial_request: Option<StorefrontRequest>,
}
impl Drop for BrowserSession {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
impl BrowserSession {
    fn start(profile: Arc<BrowserProfile>) -> Result<Self> {
        let executable = browser_executable()?;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        drop(listener);
        // Use the installed browser as-is, with a fresh profile and a loopback-only connection.
        // No account profile, user-agent override, headless browser or fingerprint modifications.
        let child = match Command::new(executable)
            .arg(format!("--user-data-dir={}", profile.path.display()))
            .arg(format!("--remote-debugging-port={port}"))
            .arg("--remote-debugging-address=127.0.0.1")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-background-timer-throttling")
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                return Err(format!("Apple browser session could not start: {error}"));
            }
        };
        let mut session = Self {
            child,
            _profile: profile,
            socket: None,
            sequence: 0,
            target_id: String::new(),
            page: String::new(),
            window_id: None,
            minimized: false,
            server_retry_until: 0,
            websocket_url: String::new(),
            started_at_ms: now_ms(),
            page_loaded_at_ms: None,
            last_response: ResponseMetadata::default(),
            session_valid_until_ms: None,
            initial_request: None,
        };
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if session
                .child
                .try_wait()
                .map_err(|e| e.to_string())?
                .is_some()
            {
                return Err("The Apple browser session closed before it started.".into());
            }
            if let Ok(response) = client
                .get(format!("http://127.0.0.1:{port}/json/list"))
                .send()
            {
                if let Ok(body) = response.text() {
                    if let Ok(targets) = serde_json::from_str::<Vec<Value>>(&body) {
                        if let Some(target) = targets.iter().find(|target| {
                            target["type"] == "page" && target["url"] == "about:blank"
                        }) {
                            let ws = target["webSocketDebuggerUrl"].as_str().unwrap_or_default();
                            if !websocket_is_local(ws, port) {
                                return Err(
                                    "Apple browser session returned an invalid local connection."
                                        .into(),
                                );
                            }
                            let (mut socket, _) = connect(ws)
                                .map_err(|e| format!("Apple browser connection failed: {e}"))?;
                            if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
                                stream
                                    .set_read_timeout(Some(Duration::from_secs(1)))
                                    .map_err(|e| e.to_string())?;
                                stream
                                    .set_write_timeout(Some(Duration::from_secs(5)))
                                    .map_err(|e| e.to_string())?;
                            }
                            session.websocket_url = ws.to_string();
                            session.socket = Some(socket);
                            session.target_id =
                                target["id"].as_str().unwrap_or_default().to_string();
                            break;
                        }
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err("Apple browser session did not start in time.".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        session.rpc("Page.enable", json!({}))?;
        session.rpc("Network.enable", json!({}))?;
        if let Ok(window) = session.rpc(
            "Browser.getWindowForTarget",
            json!({"targetId":session.target_id}),
        ) {
            session.window_id = window["windowId"].as_i64();
        }
        Ok(session)
    }
    fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.rpc_with_timeout(method, params, TIMEOUT)
    }
    fn rpc_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        self.sequence += 1;
        let id = self.sequence;
        self.socket
            .as_mut()
            .ok_or("Apple browser session is disconnected.")?
            .send(Message::Text(
                json!({"id":id,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .map_err(|e| format!("Apple browser session disconnected: {e}"))?;
        let deadline = Instant::now() + timeout;
        loop {
            match self.socket.as_mut().unwrap().read() {
                Ok(Message::Text(text)) => {
                    let value: Value = serde_json::from_str(&text)
                        .map_err(|_| "Apple browser returned an unreadable message.")?;
                    if value["id"].as_u64() == Some(id) {
                        if let Some(error) = value.get("error") {
                            return Err(format!(
                                "Apple browser command failed: {}",
                                error["message"]
                            ));
                        }
                        return Ok(value["result"].clone());
                    }
                    StorefrontRequest::observe(&mut self.initial_request, &value);
                }
                Ok(Message::Close(_)) => return Err("Apple browser session closed.".into()),
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(format!("Apple browser session disconnected: {error}")),
            }
            if Instant::now() >= deadline {
                return Err(
                    "Apple browser request timed out. The next scheduled check will retry.".into(),
                );
            }
        }
    }
    fn reconnect_transport(&mut self) -> Result<()> {
        // Reconnect to the same browser target. Cookies, profile and browser identity stay intact.
        let (mut socket, _) = connect(self.websocket_url.as_str())
            .map_err(|error| format!("Apple browser connection failed: {error}"))?;
        if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .map_err(|error| error.to_string())?;
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .map_err(|error| error.to_string())?;
        }
        self.socket = Some(socket);
        self.rpc("Page.enable", json!({}))?;
        self.rpc("Network.enable", json!({}))?;
        self.page.clear();
        Ok(())
    }
    fn sample(&mut self) -> Sample {
        let page = self.rpc_with_timeout("Runtime.evaluate", json!({"expression":"({inStorefront:location.origin==='https://www.apple.com'&&['/shop/buy-iphone/','/shop/buy-ipad/','/shop/buy-mac/'].some(path=>location.pathname.startsWith(path)),ready:document.readyState,visibility:document.visibilityState,bootstrap:!!window.fulfillmentBootstrap})","returnByValue":true}), Duration::from_secs(2)).ok();
        let cookies = self
            .rpc_with_timeout(
                "Network.getCookies",
                json!({"urls":["https://www.apple.com/shop/fulfillment-messages"]}),
                Duration::from_secs(2),
            )
            .ok();
        Sample {
            page: diagnostics::page_metadata(
                page.as_ref()
                    .map(|value| &value["result"]["value"])
                    .unwrap_or(&Value::Null),
            ),
            cookies: cookies
                .as_ref()
                .map(diagnostics::cookie_metadata)
                .unwrap_or_default(),
            cookies_available: cookies.is_some(),
        }
    }
    fn event(&self, event: &str) -> ConnectionEvent {
        ConnectionEvent {
            at_ms: now_ms(),
            event: event.into(),
            profile_id: self
                ._profile
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            profile_created_at_ms: Some(self._profile.created_at_ms),
            browser_started_at_ms: Some(self.started_at_ms),
            page_loaded_at_ms: self.page_loaded_at_ms,
            ..Default::default()
        }
    }
    fn evaluate(&mut self, expression: String, await_promise: bool) -> Result<Value> {
        let value = self.rpc(
            "Runtime.evaluate",
            json!({"expression":expression,"awaitPromise":await_promise,"returnByValue":true}),
        )?;
        if value.get("exceptionDetails").is_some() {
            return Err("Apple storefront could not complete its inventory request.".into());
        }
        Ok(value["result"]["value"].clone())
    }
    fn prepare_page(&mut self, target: &crate::catalog::ResolvedTarget) -> Result<()> {
        let page = storefront(target)?;
        if self.page == page {
            return Ok(());
        }
        // Renew through the same normal browser/profile. A minimized page may not
        // complete the storefront's initialization when its session needs attention.
        self.reveal()?;
        let navigation_at = now_ms();
        self.initial_request = None;
        let navigation = self.rpc("Page.navigate", json!({"url":page}))?;
        let loader_id = navigation["loaderId"].as_str().unwrap_or_default();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let ready = self
                .evaluate(
                    format!("performance.timeOrigin >= {navigation_at} && !!window.fulfillmentBootstrap && document.readyState === 'complete'"),
                    false,
                )?
                .as_bool()
                == Some(true);
            if let Some(request) = self
                .initial_request
                .as_ref()
                .filter(|request| !loader_id.is_empty() && request.loader_id == loader_id)
            {
                if let Some(outcome) = request.outcome() {
                    if outcome.is_err() {
                        self.last_response = request.response.clone();
                        self.server_retry_until = request.response.retry_until_ms.unwrap_or(0);
                        return outcome;
                    }
                    if ready {
                        break;
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err("Apple storefront initialization timed out waiting for its initial availability request. The next scheduled check will retry.".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.page_loaded_at_ms = Some(now_ms());
        self.page = page;
        Ok(())
    }
    fn reveal(&mut self) -> Result<()> {
        if let Some(window_id) = self.window_id {
            self.rpc(
                "Browser.setWindowBounds",
                json!({"windowId":window_id,"bounds":{"windowState":"normal"}}),
            )?;
        }
        self.rpc("Page.bringToFront", json!({}))?;
        self.minimized = false;
        Ok(())
    }
    fn minimize(&mut self) {
        if !self.minimized {
            if let Some(window_id) = self.window_id {
                if self
                    .rpc(
                        "Browser.setWindowBounds",
                        json!({"windowId":window_id,"bounds":{"windowState":"minimized"}}),
                    )
                    .is_ok()
                {
                    self.minimized = true;
                }
            }
        }
    }
    fn fetch(
        &mut self,
        target: &crate::catalog::ResolvedTarget,
    ) -> Result<(Option<i64>, Vec<Inventory>)> {
        self.last_response = ResponseMetadata::default();
        self.prepare_page(target)?;
        let endpoint = serde_json::to_string(apple_fulfillment_query(target)?.as_str())
            .map_err(|e| e.to_string())?;
        let expression = format!(
            r#"(async () => {{
            if (location.origin !== 'https://www.apple.com' || !['/shop/buy-iphone/','/shop/buy-ipad/','/shop/buy-mac/'].some(path=>location.pathname.startsWith(path)))
                return {{error:'The Apple session has left its product page. The next scheduled check will retry.'}};
            const controller = new AbortController();
            const timeout = setTimeout(() => controller.abort(), 30000);
            try {{
                const response = await fetch({endpoint}, {{headers:{{Accept:'application/json'}},credentials:'same-origin',cache:'no-store',signal:controller.signal}});
                const body = await response.text();
                const contentType = (response.headers.get('content-type') || '').split(';')[0].trim();
                const title = (body.match(/<title>(.*?)<\/title>/is)?.[1] || '').toLowerCase();
                let kind = contentType === 'application/json' ? 'json' : !body ? 'empty'
                    : title.includes('page not found') ? 'not_found'
                    : title.includes('access denied') ? 'access_denied'
                    : /captcha|verification|verify/.test(title) ? 'verification'
                    : /maintenance|be right back/.test(title) ? 'maintenance'
                    : contentType === 'text/html' ? 'unexpected_html' : 'other';
                const meta = {{status:response.status,contentType:['application/json','text/html','text/plain'].includes(contentType)?contentType:'other',bodyKind:kind,bytes:new TextEncoder().encode(body).length,date:response.headers.get('date')}};
                if (!response.ok) return {{error:'Apple inventory returned HTTP ' + response.status + '. The next scheduled check will retry.',retryAfter:response.headers.get('retry-after'),response:meta}};
                if (body.length > {MAX_BODY}) return {{error:'Apple inventory response was too large.',response:meta}};
                return {{body,response:meta}};
            }} catch (error) {{ return {{error:error.name === 'AbortError'
                ? 'Apple inventory request timed out. The next scheduled check will retry.'
                : 'Apple inventory request failed: ' + String(error.message)}}; }}
            finally {{ clearTimeout(timeout); }}
        }})()"#
        );
        let response = self.evaluate(expression, true)?;
        self.last_response = diagnostics::response_metadata(&response);
        self.server_retry_until =
            retry_after_at(response.get("retryAfter").and_then(Value::as_str), now_ms());
        self.last_response.retry_until_ms =
            (self.server_retry_until > 0).then_some(self.server_retry_until);
        let mut result = inventory_reply(response);
        if self.server_retry_until > now_ms() {
            if let Err(error) = &mut result {
                error.push_str(" Apple supplied Retry-After; wait until the next scheduled check.");
            }
        }
        result
    }
}
impl AppleSession {
    pub fn configure_diagnostics(&self, directory: &Path) {
        self.diagnostics.configure(directory);
    }
    pub fn connection_report(&self) -> ConnectionReport {
        self.diagnostics.report()
    }
    pub fn export_diagnostics(&self) -> Result<String> {
        self.diagnostics.export()
    }
    pub fn snapshot_saved(&self, id: i64, due: i64) {
        self.diagnostics.snapshot_saved(id, due);
    }
    pub fn fetch(
        &self,
        target: &crate::catalog::ResolvedTarget,
        mode: crate::core::ConnectionMode,
    ) -> Result<(Option<i64>, Vec<Inventory>)> {
        self.fetch_with_mode(target, mode, false)
    }
    pub fn reconnect_and_fetch(
        &self,
        target: &crate::catalog::ResolvedTarget,
        mode: crate::core::ConnectionMode,
    ) -> Result<(Option<i64>, Vec<Inventory>)> {
        self.fetch_with_mode(target, mode, true)
    }
    fn fetch_with_mode(
        &self,
        target: &crate::catalog::ResolvedTarget,
        mode: crate::core::ConnectionMode,
        reconnect: bool,
    ) -> Result<(Option<i64>, Vec<Inventory>)> {
        let started = now_ms();
        let check_id = format!("{}-{started}", std::process::id());
        if mode != crate::core::ConnectionMode::Browser {
            let (direct, response) = crate::apple_http::fetch(target);
            self.server_retry_until
                .store(response.retry_until_ms.unwrap_or(0), Ordering::SeqCst);
            let fallback = mode == crate::core::ConnectionMode::Auto
                && direct.is_err()
                && crate::apple_http::browser_fallback_allowed(&response);
            self.diagnostics.record(ConnectionEvent {
                at_ms: now_ms(),
                event: if fallback {
                    "endpoint_unavailable"
                } else {
                    "inventory_check"
                }
                .into(),
                check_id: Some(check_id.clone()),
                operation: Some(if reconnect { "reconnect" } else { "check" }.into()),
                success: Some(direct.is_ok()),
                error_kind: direct
                    .as_ref()
                    .err()
                    .map(|error| diagnostics::error_kind(error).into()),
                inventory_rows: direct.as_ref().ok().map(|(_, rows)| rows.len()),
                elapsed_ms: Some(now_ms().saturating_sub(started).max(0) as u64),
                response,
                ..Default::default()
            });
            if !fallback {
                return direct;
            }
            // Only a removed endpoint warrants the storefront alternative. Refusals and
            // rate limits remain failed checks with backoff; never rotate routes for them.
        }
        let mut state = self
            .session
            .lock()
            .map_err(|_| "Apple browser session lock failed.")?;
        if state
            .as_mut()
            .is_some_and(|session| session.child.try_wait().ok().flatten().is_some())
        {
            state.take();
            self.manual_open.store(false, Ordering::SeqCst);
            self.pid.store(0, Ordering::SeqCst);
        }
        if state.is_none() {
            let session = match BrowserSession::start(self.profile_handle()?) {
                Ok(session) => session,
                Err(error) => {
                    self.diagnostics.record(ConnectionEvent {
                        at_ms: now_ms(),
                        event: "inventory_check".into(),
                        check_id: Some(check_id),
                        success: Some(false),
                        error_kind: Some(diagnostics::error_kind(&error).into()),
                        ..Default::default()
                    });
                    return Err(error);
                }
            };
            self.diagnostics.record(session.event("browser_connected"));
            self.pid.store(session.child.id(), Ordering::SeqCst);
            *state = Some(session);
        }
        let session = state.as_mut().unwrap();
        let before = session.sample();
        session.last_response = ResponseMetadata::default();
        let observed_expiry = session_valid_until(&before).or(session.session_valid_until_ms);
        let refresh = !reconnect
            && session_renewal_due(
                observed_expiry,
                started,
                self.manual_open.load(Ordering::SeqCst),
            );
        if refresh {
            let mut event = session.event("session_renewal_requested");
            event.before = Some(before.clone());
            event.check_id = Some(check_id.clone());
            self.diagnostics.record(event);
            // Do not replace or alter verification cookies. Once their issued lifetime
            // ends, load the normal storefront so its own verification can run again.
            if let Some(expiry) = observed_expiry {
                let remaining = expiry.saturating_sub(now_ms());
                if remaining > 0 {
                    std::thread::sleep(Duration::from_millis(
                        remaining.min(RENEWAL_MARGIN_MS) as u64
                    ));
                }
            }
            session.page.clear();
        }
        if reconnect {
            let mut event = session.event("reconnect_requested");
            event.before = Some(before.clone());
            event.check_id = Some(check_id.clone());
            self.diagnostics.record(event);
        }
        let result = if reconnect {
            session
                .reconnect_transport()
                .and_then(|_| session.fetch(target))
        } else {
            session.fetch(target)
        };
        let after = session.sample();
        if after.cookies_available {
            let current_expiry = session_valid_until(&after);
            session.session_valid_until_ms = if result.is_ok() {
                current_expiry
            } else {
                current_expiry.or(observed_expiry)
            };
        }
        let mut event = session.event("inventory_check");
        event.check_id = Some(check_id);
        event.operation = Some(
            if reconnect {
                "reconnect"
            } else if refresh {
                "session_renewal"
            } else {
                "check"
            }
            .into(),
        );
        event.elapsed_ms = Some(now_ms().saturating_sub(started).max(0) as u64);
        event.success = Some(result.is_ok());
        event.error_kind = result
            .as_ref()
            .err()
            .map(|error| diagnostics::error_kind(error).into());
        event.inventory_rows = result.as_ref().ok().map(|(_, rows)| rows.len());
        event.response = session.last_response.clone();
        event.response.endpoint = Some("/shop/fulfillment-messages".into());
        event.response.transport = Some("browser".into());
        event.before = Some(before);
        event.after = Some(after);
        self.diagnostics.record(event);
        if let Some(session) = state.as_mut() {
            self.server_retry_until
                .store(session.server_retry_until, Ordering::SeqCst);
            if !self.manual_open.load(Ordering::SeqCst) {
                session.minimize();
            }
        }
        if let Err(error) = &result {
            eprintln!("Apple pickup session: {error}");
            if error.contains("disconnected")
                || error.contains("session closed")
                || error.starts_with("Apple browser request timed out")
            {
                state.take();
                self.pid.store(0, Ordering::SeqCst);
            } else if !self.manual_open.load(Ordering::SeqCst) {
                if let Some(session) = state.as_mut() {
                    session.page.clear();
                }
            }
        }
        result
    }
    fn profile_handle(&self) -> Result<Arc<BrowserProfile>> {
        let mut profile = self
            .profile
            .lock()
            .map_err(|_| "Apple profile lock failed.")?;
        if profile.is_none() {
            *profile = Some(Arc::new(BrowserProfile::create()?));
        }
        Ok(profile.as_ref().unwrap().clone())
    }
    pub fn retry_until(&self) -> i64 {
        self.server_retry_until.load(Ordering::SeqCst)
    }
    pub fn restore_retry_until(&self, until: i64) {
        self.server_retry_until.store(until, Ordering::SeqCst);
    }
    pub fn is_manual_open(&self) -> bool {
        self.manual_open.load(Ordering::SeqCst)
    }
    pub fn open(&self, target: &crate::catalog::ResolvedTarget) -> Result<()> {
        let mut state = self
            .session
            .lock()
            .map_err(|_| "Apple browser session lock failed.")?;
        if state
            .as_mut()
            .is_some_and(|session| session.child.try_wait().ok().flatten().is_some())
        {
            state.take();
        }
        if state.is_none() {
            let session = BrowserSession::start(self.profile_handle()?)?;
            self.pid.store(session.child.id(), Ordering::SeqCst);
            *state = Some(session);
        }
        let session = state.as_mut().unwrap();
        session.reveal()?;
        // Keep the session available to the user even when Apple's initialization fails.
        self.manual_open.store(true, Ordering::SeqCst);
        if self.retry_until() <= now_ms() {
            let result = session.prepare_page(target);
            self.server_retry_until
                .store(session.server_retry_until, Ordering::SeqCst);
            result?;
        }
        Ok(())
    }
    pub fn hide(&self) -> Result<()> {
        let mut state = self
            .session
            .lock()
            .map_err(|_| "Apple browser session lock failed.")?;
        self.manual_open.store(false, Ordering::SeqCst);
        if let Some(session) = state.as_mut() {
            session.minimize();
        }
        Ok(())
    }
    pub fn shutdown(&self) {
        if let Ok(mut state) = self.session.try_lock() {
            state.take();
            self.pid.store(0, Ordering::SeqCst);
            if let Ok(mut profile) = self.profile.lock() {
                profile.take();
            }
            return;
        }
        let pid = self.pid.swap(0, Ordering::SeqCst);
        // Interrupt our own child if collection is in progress, allowing its socket read to finish.
        if pid != 0 {
            let _ = Command::new("/bin/kill")
                .args(["-TERM", &pid.to_string()])
                .status();
        }
        if let Ok(mut state) = self.session.lock() {
            state.take();
        }
        if let Ok(mut profile) = self.profile.lock() {
            profile.take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::DEFAULT_URL;
    fn initial_response(status: u16) -> Value {
        json!({"method":"Network.responseReceived","params":{
            "requestId":"initial","loaderId":"new-document","response":{
                "url":"https://www.apple.com/shop/fulfillment-messages?parts.0=example",
                "status":status,"headers":{
                    "Content-Type":"application/json; charset=utf-8",
                    "Retry-After":"120",
                    "Set-Cookie":"SECRET_VALUE",
                    "Date":"Sat, 31 Oct 2026 22:00:00 GMT"
                }
            }
        }})
    }
    #[test]
    fn initial_http_failure_preserves_status_and_retry_delay_without_waiting_for_completion() {
        let mut request = None;
        StorefrontRequest::observe(&mut request, &initial_response(541));
        let request = request.unwrap();
        assert_eq!(request.loader_id, "new-document");
        assert!(!request.completed);
        let error = request.outcome().unwrap().unwrap_err();
        assert!(error.contains("HTTP 541"));
        assert!(error.contains("Retry-After"));
        assert!(request.response.retry_until_ms.unwrap() >= now_ms() + 119000);
        let serialized = serde_json::to_string(&request.response).unwrap();
        assert!(!serialized.contains("SECRET_VALUE"));
        assert_eq!(
            request.response.content_type.as_deref(),
            Some("application/json")
        );
    }
    #[test]
    fn initial_success_requires_its_own_completed_request_and_ignores_unrelated_traffic() {
        let mut request = None;
        StorefrontRequest::observe(&mut request, &initial_response(200));
        assert!(request.as_ref().unwrap().outcome().is_none());
        StorefrontRequest::observe(
            &mut request,
            &json!({"method":"Network.loadingFinished","params":{"requestId":"unrelated"}}),
        );
        assert!(request.as_ref().unwrap().outcome().is_none());
        let mut unrelated = initial_response(503);
        unrelated["params"]["response"]["url"] =
            json!("https://example.com/shop/fulfillment-messages");
        StorefrontRequest::observe(&mut request, &unrelated);
        assert_eq!(request.as_ref().unwrap().response.http_status, Some(200));
        StorefrontRequest::observe(
            &mut request,
            &json!({"method":"Network.loadingFinished","params":{"requestId":"initial"}}),
        );
        assert!(request.as_ref().unwrap().outcome().unwrap().is_ok());
    }
    #[test]
    fn failed_initial_connection_is_detected_even_without_an_http_response() {
        let mut request = None;
        StorefrontRequest::observe(
            &mut request,
            &json!({"method":"Network.requestWillBeSent","params":{
                "requestId":"initial","loaderId":"new-document","request":{"url":"https://www.apple.com/shop/fulfillment-messages"}
            }}),
        );
        StorefrontRequest::observe(
            &mut request,
            &json!({"method":"Network.loadingFailed","params":{"requestId":"initial","errorText":"private detail"}}),
        );
        let request = request.unwrap();
        assert_eq!(request.response.http_status, None);
        assert_eq!(request.outcome().unwrap().unwrap_err(), "Apple storefront's initial availability request failed. The next scheduled check will retry.");
    }
    #[test]
    fn renewal_checks_issued_expiry_and_leaves_manual_sessions_under_user_control() {
        let expiry = 10_000;
        assert!(!session_renewal_due(
            Some(expiry),
            expiry - RENEWAL_MARGIN_MS - 1,
            false
        ));
        assert!(session_renewal_due(
            Some(expiry),
            expiry - RENEWAL_MARGIN_MS,
            false
        ));
        assert!(session_renewal_due(Some(expiry), expiry + 1, false));
        assert!(!session_renewal_due(Some(expiry), expiry + 1, true));
        assert!(!session_renewal_due(None, expiry + 1, false));
        let sample = Sample {
            cookies: diagnostics::cookie_metadata(&json!({"cookies":[
                {"name":"analytics","domain":"apple.com","expires":1.0},
                {"name":"shld_bt_m","domain":"apple.com","expires":20.0},
                {"name":"shld_bt_ck","domain":"apple.com","expires":30.0}
            ]})),
            cookies_available: true,
            ..Default::default()
        };
        assert_eq!(session_valid_until(&sample), Some(20_000));
    }
    #[test]
    fn browser_session_uses_supported_product_and_local_connection_only() {
        assert_eq!(storefront(&crate::catalog::builtin().resolve(&crate::catalog::Target::from_key(DEFAULT_URL).unwrap()).unwrap()).unwrap(), "https://www.apple.com/shop/buy-iphone/iphone-duo/7.6-inch-display-1tb-night-sky-unlocked");
        assert!(websocket_is_local(
            "ws://127.0.0.1:43210/devtools/page/owned",
            43210
        ));
        for bad in [
            "ws://example.com:43210/devtools/page/owned",
            "wss://127.0.0.1:43210/devtools/page/owned",
            "ws://127.0.0.1:43211/devtools/page/owned",
            "ws://user@127.0.0.1:43210/devtools/page/owned",
            "ws://127.0.0.1:43210/unexpected",
        ] {
            assert!(!websocket_is_local(bad, 43210));
        }
    }
    #[test]
    fn profile_survives_transport_replacement_until_app_shutdown() {
        let session = AppleSession::default();
        let profile = session.profile_handle().unwrap();
        let path = profile.path.clone();
        drop(profile);
        assert!(path.is_dir());
        let replacement = session.profile_handle().unwrap();
        assert_eq!(replacement.path, path);
        drop(replacement);
        session.shutdown();
        assert!(!path.exists());
    }
    #[test]
    fn retry_after_accepts_seconds_and_http_dates() {
        assert_eq!(retry_after_at(Some("120"), 1000), 121000);
        let date = chrono::DateTime::parse_from_rfc2822("Thu, 29 Oct 2026 21:30:00 GMT")
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            retry_after_at(Some("Thu, 29 Oct 2026 21:30:00 GMT"), date - 1000),
            date
        );
        assert_eq!(
            retry_after_at(Some("Thu, 29 Oct 2026 21:30:00 GMT"), date + 1000),
            0
        );
        assert_eq!(retry_after_at(None, 1000), 0);
        assert_eq!(retry_after_at(Some("invalid"), 1000), 0);
    }
    #[test]
    fn browser_results_preserve_inventory_and_do_not_hide_failures() {
        let response = include_str!("../tests/fixtures/apple-fulfillment.json");
        let (_, rows) = inventory_reply(json!({"body":response})).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].part_number, "MK1L4LL/A");
        assert_eq!(
            inventory_reply(json!({"error":"HTTP 541"})).unwrap_err(),
            "HTTP 541"
        );
        assert!(inventory_reply(json!({"body":"<html>Unavailable</html>"})).is_err());
        assert!(inventory_reply(json!({})).is_err());
    }
}
