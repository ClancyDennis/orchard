use crate::apple_session::retry_after_at;
use crate::connection_diagnostics::ResponseMetadata;
use crate::core::{apple_query, now_ms, parse_apple_inventory, Inventory, Result};
use std::io::Read;
use std::time::Duration;

const MAX_BODY: u64 = 4 * 1024 * 1024;

pub fn fetch(
    target: &crate::catalog::ResolvedTarget,
) -> (Result<(Option<i64>, Vec<Inventory>)>, ResponseMetadata) {
    let url = match apple_query(target) {
        Ok(url) => url,
        Err(error) => return (Err(error), ResponseMetadata::default()),
    };
    fetch_url(&url)
}

fn fetch_url(url: &reqwest::Url) -> (Result<(Option<i64>, Vec<Inventory>)>, ResponseMetadata) {
    let mut metadata = ResponseMetadata {
        endpoint: Some("/shop/retail/pickup-message".into()),
        transport: Some("http".into()),
        ..Default::default()
    };
    let result = (|| {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("OrchardInventoryTracker/0.1")
            .build()
            .map_err(|error| error.to_string())?;
        // The public pickup endpoint currently accepts ordinary HTTP requests.
        // Use our own identity, without browser credentials or cookie manipulation.
        let response = client
            .get(url.clone())
            .header("Accept", "application/json")
            .header("Cache-Control", "no-cache")
            .send()
            .map_err(|error| {
                if error.is_timeout() {
                    "Apple inventory request timed out. The next scheduled check will retry.".into()
                } else {
                    format!("Apple inventory request failed: {error}")
                }
            })?;
        let status = response.status();
        metadata.http_status = Some(status.as_u16());
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|header| header.to_str().ok())
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim();
        metadata.content_type = Some(
            if ["application/json", "text/html", "text/plain"].contains(&content_type) {
                content_type
            } else {
                "other"
            }
            .into(),
        );
        metadata.server_date_ms = response
            .headers()
            .get("date")
            .and_then(|header| header.to_str().ok())
            .and_then(|date| chrono::DateTime::parse_from_rfc2822(date).ok())
            .map(|date| date.timestamp_millis());
        let retry_until = retry_after_at(
            response
                .headers()
                .get("retry-after")
                .and_then(|header| header.to_str().ok()),
            now_ms(),
        );
        metadata.retry_until_ms = (retry_until > now_ms()).then_some(retry_until);
        if !status.is_success() {
            let mut error = format!(
                "Apple inventory returned HTTP {}. The next scheduled check will retry.",
                status.as_u16()
            );
            if metadata.retry_until_ms.is_some() {
                error.push_str(" Apple supplied Retry-After; wait until the next scheduled check.");
            }
            return Err(error);
        }
        let mut body = Vec::new();
        response
            .take(MAX_BODY + 1)
            .read_to_end(&mut body)
            .map_err(|_| {
                "Apple inventory request failed while reading its response.".to_string()
            })?;
        metadata.body_bytes = Some(body.len() as u64);
        if body.len() as u64 > MAX_BODY {
            return Err("Apple inventory response was too large.".into());
        }
        metadata.body_kind = Some(
            if body.is_empty() {
                "empty"
            } else if metadata.content_type.as_deref() == Some("application/json") {
                "json"
            } else if metadata.content_type.as_deref() == Some("text/html") {
                "unexpected_html"
            } else {
                "other"
            }
            .into(),
        );
        let text = std::str::from_utf8(&body)
            .map_err(|_| "Apple inventory returned an unreadable response.".to_string())?;
        parse_apple_inventory(text)
    })();
    (result, metadata)
}

pub fn browser_fallback_allowed(metadata: &ResponseMetadata) -> bool {
    matches!(metadata.http_status, Some(404 | 410))
        && metadata
            .retry_until_ms
            .is_none_or(|until| until <= now_ms())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    fn serve(status: &str, headers: &str, body: &str) -> reqwest::Url {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{headers}\r\n{body}",
            body.len()
        );
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]).to_lowercase();
            assert!(request.contains("user-agent: orchardinventorytracker/0.1"));
            assert!(!request.contains("cookie:"));
            stream.write_all(response.as_bytes()).unwrap();
        });
        reqwest::Url::parse(&format!("http://{address}/inventory")).unwrap()
    }

    #[test]
    fn http_collection_parses_inventory_and_retains_public_diagnostics() {
        let fixture = include_str!("../tests/fixtures/apple-retail-pickup.json");
        let (result, metadata) = fetch_url(&serve(
            "200 OK",
            "Content-Type: application/json; charset=utf-8\r\n",
            fixture,
        ));
        let (_, rows) = result.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].part_number, "MK1F4LL/A");
        assert_eq!(rows[0].product, "iPhone Duo 256GB Night Sky");
        assert_eq!(metadata.http_status, Some(200));
        assert_eq!(
            metadata.endpoint.as_deref(),
            Some("/shop/retail/pickup-message")
        );
        assert_eq!(metadata.transport.as_deref(), Some("http"));
        assert_eq!(metadata.body_kind.as_deref(), Some("json"));
        assert_eq!(metadata.body_bytes, Some(fixture.len() as u64));
    }

    #[test]
    fn refusal_retains_retry_after_and_does_not_switch_endpoints() {
        let (result, metadata) = fetch_url(&serve(
            "541 Rejected",
            "Retry-After: 120\r\nSet-Cookie: SECRET\r\n",
            "blocked",
        ));
        let error = result.unwrap_err();
        assert!(error.contains("HTTP 541"));
        assert!(error.contains("Retry-After"));
        assert!(metadata.retry_until_ms.unwrap() >= now_ms() + 119000);
        assert!(!browser_fallback_allowed(&metadata));
        assert!(!serde_json::to_string(&metadata).unwrap().contains("SECRET"));
        for status in [200, 403, 429, 503, 541] {
            assert!(!browser_fallback_allowed(&ResponseMetadata {
                http_status: Some(status),
                ..Default::default()
            }));
        }
        assert!(browser_fallback_allowed(&ResponseMetadata {
            http_status: Some(404),
            ..Default::default()
        }));
        assert!(!browser_fallback_allowed(&ResponseMetadata {
            http_status: Some(410),
            retry_until_ms: Some(now_ms() + 10000),
            ..Default::default()
        }));
    }

    #[test]
    fn pickup_response_preserves_available_and_unavailable_configurations() {
        let mut fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/apple-retail-pickup.json"))
                .unwrap();
        fixture["body"]["stores"][0]["partsAvailability"]["MK1F4LL/A"]["pickupDisplay"] =
            serde_json::json!("available");
        let body = fixture.to_string();
        let (result, _) = fetch_url(&serve(
            "200 OK",
            "Content-Type: application/json\r\n",
            &body,
        ));
        let (_, rows) = result.unwrap();
        assert_eq!(rows[0].status, "available");
        assert_eq!(rows[1].status, "unavailable");
    }

    #[test]
    fn successful_http_with_invalid_inventory_remains_a_failure() {
        for body in [
            "<html>Unavailable</html>",
            r#"{"head":{"status":"541"},"body":{"error":"Error"}}"#,
        ] {
            let (result, metadata) =
                fetch_url(&serve("200 OK", "Content-Type: text/html\r\n", body));
            assert!(result.is_err());
            assert_eq!(metadata.http_status, Some(200));
            assert!(!browser_fallback_allowed(&metadata));
        }
    }
}
