use crate::core::Result;
use rusqlite::{params, Connection};

pub fn is_temporary_apple_error(error: &str) -> bool {
    ["HTTP 541", "HTTP 429", "HTTP 503", "status \"541\""]
        .iter()
        .any(|code| error.contains(code))
        || error.starts_with("Apple storefront initialization timed out")
        || error.starts_with("Apple storefront did not finish its initial availability request")
        || error.starts_with("Apple storefront's initial availability request failed")
        || error.starts_with("Apple inventory request timed out")
        || error.starts_with("Apple inventory request failed")
        || (error.starts_with("Apple browser")
            && [
                "disconnected",
                "session closed",
                "timed out",
                "connection failed",
            ]
            .iter()
            .any(|message| error.contains(message)))
}

pub fn failure_delay_ms(failures: u32, interval_seconds: u32) -> i64 {
    if failures < 2 {
        return 0;
    }
    let base = (interval_seconds as i64 * 2).max(300) * 1000;
    base.saturating_mul(1_i64 << (failures - 2).min(20))
        .min(3_600_000)
}

// The recorded plan retains a temporary source delay through application restarts.
pub fn saved_state(db: &Connection, url: &str) -> Result<(u32, Option<i64>, i64)> {
    let mut query = db.prepare("SELECT error,next_check_at FROM snapshots WHERE url=?1 AND source='apple' ORDER BY observed_at DESC,id DESC LIMIT 50")
        .map_err(|e| e.to_string())?;
    let mut rows = query.query(params![url]).map_err(|e| e.to_string())?;
    let mut failures = 0;
    let mut due = None;
    let mut server_until = 0;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let error: Option<String> = row.get(0).map_err(|e| e.to_string())?;
        let Some(error) = error.filter(|error| is_temporary_apple_error(error)) else {
            break;
        };
        let plan: Option<i64> = row.get(1).map_err(|e| e.to_string())?;
        if failures == 0 {
            due = plan;
            if error.contains("Apple supplied Retry-After") {
                server_until = plan.unwrap_or(0);
            }
        }
        failures += 1;
    }
    Ok((failures, due, server_until))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{initialize, insert_snapshot, Snapshot, Source, DEFAULT_URL};
    #[test]
    fn repeated_failures_back_off_with_a_bounded_delay() {
        assert_eq!(failure_delay_ms(0, 60), 0);
        assert_eq!(failure_delay_ms(1, 60), 0);
        assert_eq!(failure_delay_ms(2, 60), 300_000);
        assert_eq!(failure_delay_ms(3, 60), 600_000);
        assert_eq!(failure_delay_ms(2, 600), 1_200_000);
        assert_eq!(failure_delay_ms(100, 600), 3_600_000);
        assert!(is_temporary_apple_error(
            "Apple inventory returned HTTP 541"
        ));
        assert!(!is_temporary_apple_error(
            "Apple inventory returned an unexpected response"
        ));
        for error in [
            "Apple storefront did not finish its initial availability request. The next scheduled check will retry.",
            "Apple storefront initialization timed out waiting for its initial availability request.",
            "Apple storefront's initial availability request returned HTTP 541.",
            "Apple browser session disconnected: Connection reset",
            "Apple storefront's initial availability request failed.",
        ] {
            assert!(is_temporary_apple_error(error), "{error}");
        }
        assert!(!is_temporary_apple_error(
            "Apple browser returned an unreadable message."
        ));
    }
    #[test]
    fn recorded_retry_plan_survives_restart_and_success_resets_the_streak() {
        let mut db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let make = |at, error: Option<&str>, due| Snapshot {
            id: 0,
            observed_at: at,
            source_at: None,
            source: Source::Apple,
            url: DEFAULT_URL.into(),
            error: error.map(str::to_string),
            next_check_at: Some(due),
            inventory: vec![],
        };
        insert_snapshot(&mut db, &make(1000, None, 2000)).unwrap();
        insert_snapshot(&mut db, &make(2000, Some("HTTP 541"), 3000)).unwrap();
        insert_snapshot(
            &mut db,
            &make(3000, Some("HTTP 541 Apple supplied Retry-After"), 9000),
        )
        .unwrap();
        assert_eq!(
            saved_state(&db, DEFAULT_URL).unwrap(),
            (2, Some(9000), 9000)
        );
        insert_snapshot(&mut db, &make(4000, None, 5000)).unwrap();
        assert_eq!(saved_state(&db, DEFAULT_URL).unwrap(), (0, None, 0));
        insert_snapshot(
            &mut db,
            &make(
                5000,
                Some("Apple storefront did not finish its initial availability request."),
                6000,
            ),
        )
        .unwrap();
        insert_snapshot(
            &mut db,
            &make(
                6000,
                Some("Apple storefront's initial availability request returned HTTP 541."),
                306000,
            ),
        )
        .unwrap();
        assert_eq!(saved_state(&db, DEFAULT_URL).unwrap(), (2, Some(306000), 0));
    }
}
