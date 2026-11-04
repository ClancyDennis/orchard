use crate::core::Result;
use rusqlite::{params, Connection};

pub const MIN_INTERVAL_SECONDS: u32 = 60;
pub const BLOCK_COOLDOWN_MS: i64 = 15 * 60 * 1000;

#[derive(Clone, Debug)]
pub struct Gate {
    pub not_before_ms: i64,
    pub reason: String,
}

pub fn is_blocked(error: &str) -> bool {
    ["HTTP 541", "HTTP 429", "status \"541\""]
        .iter()
        .any(|code| error.contains(code))
}

pub fn block_delay_ms(failures: u32) -> i64 {
    BLOCK_COOLDOWN_MS
        .saturating_mul(1_i64 << failures.saturating_sub(1).min(20))
        .min(3_600_000)
}

pub fn effective_interval(seconds: u32) -> u32 {
    seconds.max(MIN_INTERVAL_SECONDS)
}

pub fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS apple_request_gate (
        id INTEGER PRIMARY KEY CHECK(id=1), not_before_ms INTEGER NOT NULL, reason TEXT NOT NULL);
        INSERT OR IGNORE INTO apple_request_gate(id,not_before_ms,reason) VALUES(1,0,'pacing');",
    )
    .map_err(|error| error.to_string())?;
    // Seed existing installations from their last real Apple observation. The
    // gate is global across tracker URLs so selecting another model cannot reset it.
    let last: Option<(i64, Option<String>, Option<i64>)> = db.query_row(
        "SELECT observed_at,error,next_check_at FROM snapshots WHERE source='apple' ORDER BY observed_at DESC,id DESC LIMIT 1",
        [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).optional().map_err(|error| error.to_string())?;
    if let Some((at, error, due)) = last {
        hold_until(
            db,
            at.saturating_add(MIN_INTERVAL_SECONDS as i64 * 1000),
            "pacing",
        )?;
        if error.as_deref().is_some_and(is_blocked) {
            let until = at
                .saturating_add(BLOCK_COOLDOWN_MS)
                .max(due.unwrap_or(0).min(at.saturating_add(3_600_000)));
            hold_until(db, until, "cooldown")?;
        }
    }
    Ok(())
}

use rusqlite::OptionalExtension;

pub fn read(db: &Connection) -> Result<Gate> {
    db.query_row(
        "SELECT not_before_ms,reason FROM apple_request_gate WHERE id=1",
        [],
        |row| {
            Ok(Gate {
                not_before_ms: row.get(0)?,
                reason: row.get(1)?,
            })
        },
    )
    .map_err(|error| error.to_string())
}

pub fn hold_until(db: &Connection, until: i64, reason: &str) -> Result<()> {
    db.execute(
        "UPDATE apple_request_gate SET not_before_ms=?1,reason=?2 WHERE id=1 AND not_before_ms<?1",
        params![until, reason],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

pub fn claim(db: &Connection, now: i64) -> Result<std::result::Result<(), Gate>> {
    let updated = db.execute("UPDATE apple_request_gate SET not_before_ms=?1,reason='pacing' WHERE id=1 AND not_before_ms<=?2",
        params![now.saturating_add(MIN_INTERVAL_SECONDS as i64 * 1000),now]).map_err(|error|error.to_string())?;
    if updated == 1 {
        Ok(Ok(()))
    } else {
        Ok(Err(read(db)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core;

    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        core::initialize(&db).unwrap();
        initialize(&db).unwrap();
        db
    }

    #[test]
    fn scheduled_manual_and_model_switch_checks_share_one_persisted_gate() {
        let db = database();
        assert!(claim(&db, 1000).unwrap().is_ok());
        let before = db
            .query_row("SELECT count(*) FROM snapshots", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        assert_eq!(claim(&db, 2000).unwrap().unwrap_err().not_before_ms, 61000);
        initialize(&db).unwrap();
        assert!(claim(&db, 60999).unwrap().is_err());
        assert!(claim(&db, 61000).unwrap().is_ok());
        assert_eq!(
            db.query_row("SELECT count(*) FROM snapshots", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            before
        );
        assert_eq!(effective_interval(30), 60);
        assert_eq!(effective_interval(120), 120);
    }

    #[test]
    fn cooldown_cannot_be_shortened_by_a_click_or_restart() {
        let db = database();
        hold_until(&db, 901000, "cooldown").unwrap();
        hold_until(&db, 61000, "pacing").unwrap();
        initialize(&db).unwrap();
        let gate = claim(&db, 900999).unwrap().unwrap_err();
        assert_eq!(gate.not_before_ms, 901000);
        assert_eq!(gate.reason, "cooldown");
        assert!(claim(&db, 901000).unwrap().is_ok());
        assert_eq!(block_delay_ms(1), 900000);
        assert_eq!(block_delay_ms(2), 1800000);
        assert_eq!(block_delay_ms(3), 3600000);
        assert_eq!(block_delay_ms(50), 3600000);
        hold_until(&db, 2_000_000, "server_wait").unwrap();
        hold_until(&db, 1_000_000, "cooldown").unwrap();
        assert_eq!(read(&db).unwrap().reason, "server_wait");
        assert!(claim(&db, 1_999_999).unwrap().is_err());
    }

    #[test]
    fn existing_blocked_history_seeds_cooldown_without_changing_history() {
        let db = database();
        db.execute("INSERT INTO snapshots(observed_at,url,source,error,next_check_at) VALUES(1000,'duo','apple','HTTP 541',2000)",[]).unwrap();
        initialize(&db).unwrap();
        assert_eq!(read(&db).unwrap().not_before_ms, 901000);
        assert_eq!(read(&db).unwrap().reason, "cooldown");
        assert_eq!(
            db.query_row("SELECT count(*) FROM snapshots", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
