use crate::{
    catalog::{Catalog, Target},
    core::{self, HistoryPoint, LastAvailable, Result, Snapshot},
};
use rusqlite::Connection;
use serde::Serialize;

#[derive(Clone, Serialize)]
pub struct HistoryTarget {
    pub key: String,
    pub target: Option<Target>,
    pub device_name: String,
    pub configuration: String,
    pub category: String,
    pub checks: usize,
    pub last_attempt: Option<i64>,
    pub active: bool,
}
#[derive(Serialize)]
pub struct HistoryView {
    pub targets: Vec<HistoryTarget>,
    pub selected: HistoryTarget,
    pub latest: Option<Snapshot>,
    pub last_error: Option<String>,
    pub points: Vec<HistoryPoint>,
    pub last_available: Vec<LastAvailable>,
}

pub fn targets(db: &Connection, catalog: &Catalog) -> Result<Vec<HistoryTarget>> {
    let settings = core::load_settings(db)?;
    let mut statement=db.prepare("SELECT url,COUNT(*),MAX(observed_at) FROM snapshots GROUP BY url ORDER BY MAX(observed_at) DESC,url").map_err(|e|e.to_string())?;
    let entries = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? as usize,
                row.get::<_, Option<i64>>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for entry in entries {
        let (key, checks, last_attempt) = entry.map_err(|e| e.to_string())?;
        rows.push(describe(catalog, key, checks, last_attempt, &settings.url));
    }
    if !rows.iter().any(|r| r.active) {
        rows.insert(
            0,
            describe(catalog, settings.url.clone(), 0, None, &settings.url),
        );
    }
    rows.sort_by_key(|r| !r.active);
    Ok(rows)
}
fn describe(
    catalog: &Catalog,
    key: String,
    checks: usize,
    last_attempt: Option<i64>,
    active: &str,
) -> HistoryTarget {
    let target = Target::from_key(&key).ok();
    let device = target
        .as_ref()
        .and_then(|t| catalog.devices.iter().find(|d| d.id == t.device_id));
    let device_name = device
        .map(|d| d.name.clone())
        .or_else(|| target.as_ref().map(|t| t.device_id.clone()))
        .unwrap_or_else(|| "Imported history".into());
    let category = device.map(|d| d.category.clone()).unwrap_or_default();
    let configuration = target
        .as_ref()
        .map(|t| {
            if t.part_number.is_empty() {
                "All configurations".into()
            } else {
                device
                    .and_then(|d| {
                        d.variants
                            .iter()
                            .find(|v| v.part_number == t.part_number && v.carrier == t.carrier)
                    })
                    .map(|v| v.title.clone())
                    .unwrap_or_else(|| t.part_number.clone())
            }
        })
        .unwrap_or_else(|| key.clone());
    HistoryTarget {
        active: key == active,
        key,
        target,
        device_name,
        configuration,
        category,
        checks,
        last_attempt,
    }
}
pub fn read(
    db: &Connection,
    catalog: &Catalog,
    key: Option<&str>,
    start: i64,
    end: i64,
    store: &str,
    product: &str,
) -> Result<HistoryView> {
    let targets = targets(db, catalog)?;
    let selected = match key.filter(|k| !k.is_empty()) {
        Some(key) => targets.iter().find(|t| t.key == key),
        None => targets.iter().find(|t| t.active),
    }
    .ok_or("This device has no saved history.")?
    .clone();
    Ok(HistoryView {
        latest: core::latest_snapshot(db, &selected.key)?,
        last_error: core::last_error(db, &selected.key)?,
        points: core::history(db, &selected.key, start, end, store, product)?,
        last_available: core::last_available(db, &selected.key, store, product)?,
        targets,
        selected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Inventory, Source};
    #[test]
    fn device_views_keep_failures_dates_and_filters_separate_without_switching_collection() {
        let mut db = Connection::open_in_memory().unwrap();
        core::initialize(&db).unwrap();
        let catalog = crate::catalog::builtin();
        let phone = Target::from_key(core::DEFAULT_URL).unwrap();
        let settings = core::Settings {
            target: phone.clone(),
            url: phone.key(),
            ..Default::default()
        };
        core::store_settings(&db, &settings).unwrap();
        let mac = Target {
            device_id: "mac-mini".into(),
            zip_code: "10001".into(),
            part_number: "MHQK4LL/A".into(),
            carrier: "unlocked".into(),
        };
        let make = |at, target: &Target, error: Option<&str>| Snapshot {
            id: 0,
            observed_at: at,
            source_at: None,
            source: Source::Apple,
            url: target.key(),
            error: error.map(str::to_string),
            next_check_at: Some(at + 60000),
            inventory: if error.is_some() {
                vec![]
            } else {
                vec![Inventory {
                    store: "Example Store".into(),
                    store_id: "R001".into(),
                    city: "Example".into(),
                    distance: "1 mi".into(),
                    product: target.device_id.clone(),
                    part_number: if target.part_number.is_empty() {
                        "MK1G4LL/A".into()
                    } else {
                        target.part_number.clone()
                    },
                    status: "available".into(),
                    pickup_quote: "Today".into(),
                }]
            },
        };
        let phone_id = core::insert_snapshot(&mut db, &make(1000, &phone, None)).unwrap();
        let mac_id = core::insert_snapshot(&mut db, &make(2000, &mac, None)).unwrap();
        core::insert_snapshot(&mut db, &make(3000, &phone, Some("HTTP 541"))).unwrap();
        let view = read(&db, &catalog, Some(&mac.key()), 0, 5000, "R001", "").unwrap();
        assert_eq!(view.latest.unwrap().id, mac_id);
        assert!(view.last_error.is_none());
        assert_eq!(view.points.len(), 1);
        assert_eq!(view.last_available.len(), 1);
        assert!(!view.selected.active);
        assert_eq!(view.targets.len(), 2);
        assert_eq!(core::load_settings(&db).unwrap().url, phone.key());
        let view = read(&db, &catalog, None, 0, 5000, "", "").unwrap();
        assert_eq!(view.latest.unwrap().id, phone_id);
        assert_eq!(view.points.len(), 2);
        assert!(view.last_error.is_some());
        assert!(read(&db, &catalog, Some(&mac.key()), 2500, 5000, "", "")
            .unwrap()
            .points
            .is_empty());
        assert_eq!(
            read(&db, &catalog, Some(&mac.key()), 0, 5000, "R999", "")
                .unwrap()
                .points[0]
                .total,
            0
        );
    }
    #[test]
    fn all_configurations_exact_skus_and_zip_codes_are_distinct_even_if_not_in_catalog() {
        let db = Connection::open_in_memory().unwrap();
        core::initialize(&db).unwrap();
        for key in [
            "apple://us/10001/mac-mini/unlocked",
            "apple://us/10001/mac-mini/unlocked?part=MHQK4LL%2FA",
            "apple://us/94025/mac-mini/unlocked?part=MHQK4LL%2FA",
            "apple://us/94025/older-device/unlocked?part=OLD99LL%2FA",
        ] {
            db.execute(
                "INSERT INTO snapshots(url,observed_at,error) VALUES(?1,1000,'Failed')",
                [key],
            )
            .unwrap();
        }
        let rows = targets(&db, &crate::catalog::builtin()).unwrap();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows.iter().filter(|r| r.active).count(), 1);
        let old = rows
            .iter()
            .find(|r| r.device_name == "older-device")
            .unwrap();
        assert_eq!(old.configuration, "OLD99LL/A");
        let view = read(
            &db,
            &crate::catalog::builtin(),
            Some(&old.key),
            0,
            5000,
            "",
            "",
        )
        .unwrap();
        assert!(view.latest.is_none());
        assert!(view.last_error.is_some());
    }
}
