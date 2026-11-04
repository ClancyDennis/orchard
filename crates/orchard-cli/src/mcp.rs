use crate::{run_tool, Service};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, Write},
    sync::Arc,
};

fn tools() -> Value {
    let text = |description: &str| json!({"type":"string","description":description});
    let target = json!({"device_id":text("Device ID from catalog_list."),"zip_code":text("Five-digit US ZIP code."),"part_number":text("Optional exact SKU from catalog_list; omit to check all configurations."),"carrier":{"type":"string","enum":["unlocked","att","tmobile","verizon"]}});
    let mut check = target.as_object().unwrap().clone();
    check.insert(
        "connection".into(),
        json!({"type":"string","enum":["auto","direct","browser"]}),
    );
    check.insert(
        "interval_seconds".into(),
        json!({"type":"integer","minimum":1,"maximum":86400}),
    );
    check.insert(
        "limit".into(),
        json!({"type":"integer","minimum":1,"maximum":1000}),
    );
    let mut latest = target.as_object().unwrap().clone();
    latest.insert(
        "limit".into(),
        json!({"type":"integer","minimum":1,"maximum":1000}),
    );
    let mut history = latest.clone();
    for field in ["start_ms", "end_ms"] {
        history.insert(field.into(),json!({"type":"integer","description":"UTC Unix milliseconds; defaults to the last week."}));
    }
    for field in ["store_id", "product"] {
        history.insert(field.into(), text("Optional history filter."));
    }
    let tool = |name: &str,
                description: &str,
                properties: Value,
                read_only: bool,
                open_world: bool| json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"additionalProperties":false},"annotations":{"readOnlyHint":read_only,"destructiveHint":false,"openWorldHint":open_world}});
    json!([
        tool("catalog_list","List supported Apple devices. Pass device_id to get exact configurations and purchase URLs. No inventory request.",json!({"category":{"type":"string","enum":["iphone","ipad","mac"]},"device_id":text("Optional device to expand."),"carrier":{"type":"string","enum":["unlocked","att","tmobile","verizon"]}}),true,false),
        tool("catalog_refresh","Refresh a category from Apple buy pages, or add an Apple US buy page. Failed refreshes preserve the last good catalog.",json!({"category":{"type":"string","enum":["iphone","ipad","mac"]},"apple_url":text("Optional https://www.apple.com/shop/buy-* family page.")}),false,true),
        tool("inventory_status","Read local desktop settings, next allowed check, cooldown, and connection diagnostics. Does not query Apple.",json!({}),true,false),
        tool("inventory_check","Make one Apple pickup check and save the real observation. Deferred/failed results mean unknown, never out of stock. Honor retry_at_ms. Browser mode requires an installed normal browser; it shares the same pacing and cooldown.",Value::Object(check),false,true),
        tool("inventory_latest","Read the last saved inventory without a network request. Check state and observed_at: unknown/pending and historical results do not establish current stock.",Value::Object(latest),true,false),
        tool("inventory_history","Read saved observations newest first, including failure gaps. No invented history and no network request.",Value::Object(history),true,false)
    ])
}

fn response(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

pub fn serve(service: Arc<Service>) {
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.is_empty() {
            continue;
        }
        let message = serde_json::from_str::<Value>(&line);
        let reply = match message {
            Err(_) => Some(error(Value::Null, -32700, "Invalid JSON")),
            Ok(request) => {
                let id = request.get("id").cloned();
                if id.is_none() {
                    None
                } else if request["jsonrpc"] != "2.0" {
                    Some(error(id.unwrap(), -32600, "Expected JSON-RPC 2.0"))
                } else {
                    let id = id.unwrap();
                    match request["method"].as_str() {
                        Some("initialize") => {
                            let offered = request["params"]["protocolVersion"]
                                .as_str()
                                .unwrap_or("2025-11-25");
                            let version =
                                if ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"]
                                    .contains(&offered)
                                {
                                    offered
                                } else {
                                    "2025-11-25"
                                };
                            Some(response(
                                id,
                                json!({"protocolVersion":version,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"orchard","version":env!("CARGO_PKG_VERSION")},"instructions":"Inventory failures and deferred checks mean unknown. Honor retry_at_ms; desktop and MCP share request pacing through their database. This service checks pickup availability; it never purchases or reserves products."}),
                            ))
                        }
                        Some("ping") => Some(response(id, json!({}))),
                        Some("tools/list") => Some(response(id, json!({"tools":tools()}))),
                        Some("tools/call") => {
                            let name = request["params"]["name"].as_str().unwrap_or("");
                            let args = request["params"]
                                .get("arguments")
                                .cloned()
                                .unwrap_or(json!({}));
                            let (payload, is_error) = match run_tool(&service, name, &args) {
                                Ok(result) => {
                                    let failed = result["state"] == "failed";
                                    (json!({"schema_version":1,"result":result}), failed)
                                }
                                Err(error) => (json!({"schema_version":1,"error":error}), true),
                            };
                            Some(response(
                                id,
                                json!({"content":[{"type":"text","text":payload.to_string()}],"structuredContent":payload,"isError":is_error}),
                            ))
                        }
                        Some("resources/list") | Some("prompts/list") => Some(response(
                            id,
                            json!({if request["method"]=="resources/list" {"resources"} else {"prompts"}:[]}),
                        )),
                        _ => Some(error(id, -32601, "Method not supported")),
                    }
                }
            }
        };
        if let Some(reply) = reply {
            if writeln!(stdout, "{reply}")
                .and_then(|_| stdout.flush())
                .is_err()
            {
                break;
            }
        }
    }
    service.session.shutdown();
}
