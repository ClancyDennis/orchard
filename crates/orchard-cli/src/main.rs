mod mcp;
use orchard_core::{
    catalog::Target,
    core::{self, ConnectionMode, Snapshot},
    service::{CheckResult, Service},
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

fn mode(value: &str) -> Result<ConnectionMode, String> {
    match value {
        "auto" => Ok(ConnectionMode::Auto),
        "direct" => Ok(ConnectionMode::Direct),
        "browser" => Ok(ConnectionMode::Browser),
        _ => Err("connection must be auto, direct, or browser".into()),
    }
}
pub fn target(args: &Value, service: &Service) -> Result<Target, String> {
    let mut t = service.status()?.settings.target;
    if let Some(value) = string(args, "device_id")? {
        if value != t.device_id {
            t.part_number.clear();
            t.carrier = "unlocked".into();
        }
        t.device_id = value.into();
    }
    if let Some(value) = string(args, "zip_code")? {
        t.zip_code = value.into();
    }
    if let Some(value) = string(args, "part_number")? {
        t.part_number = value.into();
    }
    if let Some(value) = string(args, "carrier")? {
        t.carrier = value.into();
    }
    Ok(t)
}
pub fn string<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        _ => Err(format!("{key} must be a string")),
    }
}
fn integer(args: &Value, key: &str) -> Result<Option<i64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("{key} must be an integer")),
    }
}
fn summary(snapshot: &Snapshot, current: bool, limit: usize) -> Value {
    let available: Vec<_> = snapshot
        .inventory
        .iter()
        .filter(|r| r.status == "available")
        .collect();
    json!({"id":snapshot.id,"observed_at":snapshot.observed_at,"current":current,"availability":if snapshot.error.is_some(){"unknown"}else{"known"},"error":snapshot.error,
        "total_entries":snapshot.inventory.len(),"available_count":if snapshot.error.is_none(){Some(available.len())}else{None},
        "available":available.iter().take(limit).collect::<Vec<_>>(),"available_truncated":available.len()>limit,"next_check_at":snapshot.next_check_at})
}
pub fn check_json(result: &CheckResult, limit: usize) -> Value {
    json!({"state":result.state,"target":result.target,"snapshot":result.snapshot.as_ref().map(|s|summary(s,result.state=="checked",limit)),
        "last_known":result.last_known.as_ref().map(|s|summary(s,false,limit)),"retry_at_ms":result.retry_at_ms,"message":result.message})
}
pub fn run_tool(service: &Service, name: &str, args: &Value) -> Result<Value, String> {
    if !args.is_object() {
        return Err("Tool arguments must be an object".into());
    }
    let limit = integer(args, "limit")?.unwrap_or(100).clamp(1, 1000) as usize;
    match name {
        "catalog_list" => {
            let catalog = service.catalog();
            if let Some(id) = string(args, "device_id")? {
                let device = catalog
                    .devices
                    .into_iter()
                    .find(|d| d.id == id)
                    .ok_or("Unknown device_id. Call catalog_list to see available devices.")?;
                let carrier = string(args, "carrier")?.unwrap_or("unlocked");
                return Ok(
                    json!({"device":device.id,"name":device.name,"category":device.category,"checked_at_ms":device.checked_at_ms,"last_error":device.last_error,"configurations":device.variants.into_iter().filter(|v|v.carrier==carrier).collect::<Vec<_>>()}),
                );
            }
            let category = string(args, "category")?;
            Ok(
                json!({"devices":catalog.devices.iter().filter(|d|category.is_none_or(|c|d.category==c)).map(|d|json!({"id":d.id,"name":d.name,"category":d.category,"configuration_count":d.variants.iter().filter(|v|v.carrier=="unlocked").count(),"checked_at_ms":d.checked_at_ms,"last_error":d.last_error})).collect::<Vec<_>>() }),
            )
        }
        "catalog_refresh" => {
            let catalog = if let Some(url) = string(args, "apple_url")? {
                service.catalog.add_page(url)?
            } else {
                service
                    .catalog
                    .refresh(string(args, "category")?.unwrap_or("iphone"))?
            };
            Ok(
                json!({"devices":catalog.devices.len(),"errors":catalog.devices.iter().filter(|d|d.last_error.is_some()).map(|d|json!({"device":d.id,"error":d.last_error})).collect::<Vec<_>>() }),
            )
        }
        "inventory_status" => {
            Ok(json!({"status":service.status()?,"connection":service.session.connection_report()}))
        }
        "inventory_check" => {
            let t = target(args, service)?;
            let method = string(args, "connection")?.map(mode).transpose()?;
            let interval = integer(args, "interval_seconds")?
                .map(|n| u32::try_from(n).map_err(|_| "Invalid interval_seconds".to_string()))
                .transpose()?;
            let result = service.check(Some(t), method, interval, false)?;
            Ok(check_json(&result, limit))
        }
        "inventory_latest" => {
            let t = target(args, service)?;
            t.validate(false)?;
            let db = service.db()?;
            let snapshot = core::latest_snapshot(&db, &t.key())?;
            let error = core::last_error(&db, &t.key())?;
            let state = if error.is_some() {
                "unknown"
            } else if snapshot.is_some() {
                "known"
            } else {
                "pending"
            };
            Ok(
                json!({"target":t,"state":state,"last_error":error,"last_attempt":core::last_attempt(&db,&t.key())?,"snapshot":snapshot.as_ref().map(|s|summary(s,state=="known",limit))}),
            )
        }
        "inventory_history" => {
            let t = target(args, service)?;
            t.validate(false)?;
            let end = integer(args, "end_ms")?.unwrap_or_else(core::now_ms);
            let start = integer(args, "start_ms")?.unwrap_or(end - 7 * 86400000);
            if start > end {
                return Err("start_ms must be before end_ms".into());
            }
            let db = service.db()?;
            let points = core::history(
                &db,
                &t.key(),
                start,
                end,
                string(args, "store_id")?.unwrap_or(""),
                string(args, "product")?.unwrap_or(""),
            )?;
            let total = points.len();
            Ok(
                json!({"target":t,"points":points.into_iter().rev().take(limit).collect::<Vec<_>>(),"total":total,"truncated":total>limit}),
            )
        }
        _ => Err(format!("Unknown tool: {name}")),
    }
}

fn cli_args(args: &[String]) -> Result<(Value, bool), String> {
    let mut values = serde_json::Map::new();
    let mut full = false;
    let mut i = 0;
    while i < args.len() {
        let key = args[i].as_str();
        if key == "--full" {
            full = true;
            i += 1;
            continue;
        }
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("Missing value after {key}"))?;
        let field = match key {
            "--device" => "device_id",
            "--zip" => "zip_code",
            "--part" => "part_number",
            "--carrier" => "carrier",
            "--connection" => "connection",
            "--category" => "category",
            "--apple-url" => "apple_url",
            "--interval" => "interval_seconds",
            "--start-ms" => "start_ms",
            "--end-ms" => "end_ms",
            "--store" => "store_id",
            "--product" => "product",
            "--limit" => "limit",
            "--timeout" => "timeout",
            _ => return Err(format!("Unknown option: {key}")),
        };
        if ["interval_seconds", "start_ms", "end_ms", "limit", "timeout"].contains(&field) {
            values.insert(
                field.into(),
                json!(value
                    .parse::<i64>()
                    .map_err(|_| format!("Invalid number: {value}"))?),
            );
        } else {
            values.insert(field.into(), json!(value));
        }
        i += 2;
    }
    Ok((Value::Object(values), full))
}
fn main() {
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let mut database = orchard_core::catalog::app_data_dir().join("inventory.sqlite3");
    if let Some(i) = args.iter().position(|a| a == "--database") {
        if let Some(path) = args.get(i + 1) {
            database = PathBuf::from(path);
        } else {
            eprintln!("--database needs a path");
            std::process::exit(3);
        }
        args.drain(i..=i + 1);
    }
    let command = args.first().map(String::as_str).unwrap_or("help");
    if command == "help" || command == "--help" {
        println!("Orchard {} — Apple pickup inventory\n\nCommands: catalog, refresh, check, latest, history, status, watch, mcp\nOptions: --device ID --zip ZIP --part SKU --carrier unlocked|att|tmobile|verizon\n         --connection auto|direct|browser --interval SECONDS --timeout SECONDS\n         --category iphone|ipad|mac --apple-url APPLE_BUY_URL --database PATH\n\ncheck returns JSON; watch returns NDJSON; mcp serves MCP over stdio.\nDeferred checks never mean unavailable. Apple pacing and cooldowns are shared\nwith the desktop app when using the same database.",env!("CARGO_PKG_VERSION"));
        return;
    }
    if command == "--version" {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let service = match Service::new(&database) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("Orchard: {e}");
            std::process::exit(1);
        }
    };
    let stopped = Arc::new(AtomicBool::new(false));
    let signal = stopped.clone();
    let cleanup = service.clone();
    let _ = ctrlc::set_handler(move || {
        signal.store(true, Ordering::SeqCst);
        cleanup.session.shutdown();
        std::process::exit(0);
    });
    if command == "mcp" {
        mcp::serve(service);
        return;
    }
    let (options, full) = match cli_args(&args[1..]) {
        Ok(v) => v,
        Err(e) => {
            println!("{}", json!({"schema_version":1,"error":e}));
            std::process::exit(3);
        }
    };
    if command == "watch" {
        let interval = integer(&options, "interval_seconds")
            .ok()
            .flatten()
            .unwrap_or(120);
        let timeout = integer(&options, "timeout").ok().flatten().unwrap_or(300);
        if !(1..=86400).contains(&interval) || !(1..=86400).contains(&timeout) {
            eprintln!("interval and timeout must be 1–86400 seconds");
            std::process::exit(3);
        }
        let mut options = options;
        options["interval_seconds"] = json!(interval);
        let deadline = Instant::now() + Duration::from_secs(timeout as u64);
        loop {
            match run_tool(&service, "inventory_check", &options) {
                Ok(result) => {
                    println!(
                        "{}",
                        json!({"schema_version":1,"command":"watch","result":result})
                    );
                    if result["state"] == "checked"
                        && result["snapshot"]["available_count"]
                            .as_u64()
                            .is_some_and(|n| n > 0)
                    {
                        service.session.shutdown();
                        return;
                    }
                    let next = result["retry_at_ms"]
                        .as_i64()
                        .unwrap_or(core::now_ms() + interval * 1000);
                    while core::now_ms() < next
                        && Instant::now() < deadline
                        && !stopped.load(Ordering::SeqCst)
                    {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
                Err(error) => {
                    println!(
                        "{}",
                        json!({"schema_version":1,"command":"watch","error":error})
                    );
                    service.session.shutdown();
                    std::process::exit(1);
                }
            }
            if stopped.load(Ordering::SeqCst) || Instant::now() >= deadline {
                println!(
                    "{}",
                    json!({"schema_version":1,"command":"watch","state":"timeout"})
                );
                service.session.shutdown();
                std::process::exit(2);
            }
        }
    }
    let tool = match command {
        "catalog" => "catalog_list",
        "refresh" => "catalog_refresh",
        "check" => "inventory_check",
        "latest" => "inventory_latest",
        "history" => "inventory_history",
        "status" => "inventory_status",
        _ => {
            eprintln!("Unknown command. Run orchard --help.");
            std::process::exit(3);
        }
    };
    let result = if command == "check" && full {
        target(&options, &service)
            .and_then(|t| {
                service.check(
                    Some(t),
                    string(&options, "connection")?.map(mode).transpose()?,
                    None,
                    false,
                )
            })
            .map(|r| json!(r))
    } else {
        run_tool(&service, tool, &options)
    };
    match result {
        Ok(result) => {
            let state = result["state"].as_str().unwrap_or("checked");
            let code = if state == "failed" {
                1
            } else if state == "deferred" {
                2
            } else {
                0
            };
            println!(
                "{}",
                json!({"schema_version":1,"command":command,"result":result})
            );
            service.session.shutdown();
            std::process::exit(code);
        }
        Err(error) => {
            println!(
                "{}",
                json!({"schema_version":1,"command":command,"error":error})
            );
            service.session.shutdown();
            std::process::exit(1);
        }
    }
}
