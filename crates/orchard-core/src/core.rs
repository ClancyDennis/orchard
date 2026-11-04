use chrono::{DateTime, Timelike, Utc};
use chrono_tz::America::Los_Angeles;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

pub type Result<T> = std::result::Result<T, String>;
pub const DEFAULT_URL: &str = "apple://us/94025/iphone-duo/unlocked";

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    #[default]
    Apple,
    Checker,
}
impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Apple => "apple",
            Self::Checker => "checker",
        }
    }
    fn from_db(value: String) -> Self {
        if value == "apple" {
            Self::Apple
        } else {
            Self::Checker
        }
    }
}
fn legacy_source() -> Source {
    Source::Checker
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct TimeWindow {
    pub enabled: bool,
    pub start_minute: u32,
    pub end_minute: u32,
    pub interval_seconds: u32,
}
impl TimeWindow {
    fn contains(&self, minute: u32) -> bool {
        self.enabled
            && if self.start_minute < self.end_minute {
                minute >= self.start_minute && minute < self.end_minute
            } else {
                minute >= self.start_minute || minute < self.end_minute
            }
    }
}

#[derive(Clone, Serialize, Debug)]
pub struct Settings {
    pub target: crate::catalog::Target,
    pub connection: ConnectionMode,
    pub timezone: String,
    pub source: Source,
    pub url: String,
    pub interval_seconds: u32,
    pub enabled: bool,
    pub notify_available: bool,
    pub windows: Vec<TimeWindow>,
}

#[derive(Clone, Copy, Serialize, Deserialize, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionMode {
    #[default]
    Auto,
    Direct,
    Browser,
}
fn pacific() -> String {
    "America/Los_Angeles".into()
}

// Read earlier minute-based settings without losing a customized morning window.
impl<'de> Deserialize<'de> for Settings {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct LegacyWindow {
            enabled: bool,
            start_minute: u32,
            end_minute: u32,
            interval_minutes: u32,
        }
        #[derive(Deserialize)]
        struct StoredSettings {
            target: Option<crate::catalog::Target>,
            #[serde(default)]
            connection: ConnectionMode,
            #[serde(default = "pacific")]
            timezone: String,
            #[serde(default = "legacy_source", rename = "source")]
            _source: Source,
            url: Option<String>,
            interval_seconds: Option<u32>,
            interval_minutes: Option<u32>,
            enabled: bool,
            #[serde(default)]
            notify_available: bool,
            windows: Option<Vec<TimeWindow>>,
            morning: Option<LegacyWindow>,
        }
        let stored = StoredSettings::deserialize(deserializer)?;
        let target = match stored.target {
            Some(target) => target,
            None => crate::catalog::Target::from_key(stored.url.as_deref().unwrap_or(DEFAULT_URL))
                .map_err(serde::de::Error::custom)?,
        };
        let seconds = |minutes: u32| {
            minutes
                .checked_mul(60)
                .ok_or_else(|| serde::de::Error::custom("Interval is too large"))
        };
        let interval_seconds = match stored.interval_seconds {
            Some(value) => value,
            None => seconds(stored.interval_minutes.unwrap_or(15))?,
        };
        let windows = match stored.windows {
            Some(windows) => windows,
            None => match stored.morning {
                Some(window) => vec![TimeWindow {
                    enabled: window.enabled,
                    start_minute: window.start_minute,
                    end_minute: window.end_minute,
                    interval_seconds: seconds(window.interval_minutes)?,
                }],
                None => vec![],
            },
        };
        Ok(Self {
            url: target.key(),
            target,
            connection: stored.connection,
            timezone: stored.timezone,
            source: Source::Apple,
            interval_seconds,
            enabled: stored.enabled,
            notify_available: stored.notify_available,
            windows,
        })
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            target: crate::catalog::Target::default(),
            connection: ConnectionMode::Auto,
            timezone: pacific(),
            source: Source::Apple,
            url: crate::catalog::Target::default().key(),
            interval_seconds: 900,
            enabled: false,
            notify_available: false,
            windows: vec![],
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        self.target.validate(!self.enabled)?;
        if self.url != self.target.key() {
            return Err("The tracker identity does not match its device selection.".into());
        }
        if self.source != Source::Apple {
            return Err("New checks use Apple inventory only.".into());
        }
        self.timezone
            .parse::<chrono_tz::Tz>()
            .map_err(|_| "Choose a valid IANA time zone.".to_string())?;
        if !(1..=86_400).contains(&self.interval_seconds) {
            return Err("Choose a rate between 1 second and 24 hours.".into());
        }
        let mut occupied = [false; 1440];
        for (index, window) in self.windows.iter().enumerate() {
            if window.start_minute >= 1440
                || window.end_minute >= 1440
                || window.start_minute == window.end_minute
            {
                return Err(format!("Window {} needs different start and end times. Use the default rate for all-day collection.", index + 1));
            }
            if !(1..=86_400).contains(&window.interval_seconds) {
                return Err(format!(
                    "Window {} needs a rate between 1 second and 24 hours.",
                    index + 1
                ));
            }
            for (minute, taken) in occupied.iter_mut().enumerate() {
                if window.contains(minute as u32) {
                    if *taken {
                        return Err(format!("Window {} overlaps another enabled window. Adjust its times or disable it.", index + 1));
                    }
                    *taken = true;
                }
            }
        }
        Ok(())
    }

    fn active_window(&self, now: i64) -> Option<usize> {
        let local = DateTime::<Utc>::from_timestamp_millis(now)?.with_timezone(
            &self
                .timezone
                .parse::<chrono_tz::Tz>()
                .unwrap_or(Los_Angeles),
        );
        let minute = local.hour() * 60 + local.minute();
        self.windows
            .iter()
            .position(|window| window.contains(minute))
    }

    pub fn active_interval(&self, now: i64) -> u32 {
        self.active_window(now)
            .map(|index| self.windows[index].interval_seconds)
            .unwrap_or(self.interval_seconds)
    }

    // Scan real UTC minutes so skipped/repeated Pacific hours follow the wall clock.
    // A daily window always transitions within 48 hours, including DST changes.
    fn next_transition(&self, now: i64) -> Option<i64> {
        if !self.windows.iter().any(|window| window.enabled) {
            return None;
        }
        let active = self.active_window(now);
        let minute = now.div_euclid(60_000) * 60_000;
        (1..=2880)
            .map(|offset| minute + offset * 60_000)
            .find(|at| self.active_window(*at) != active)
    }

    fn segment_start(&self, now: i64, active: usize) -> i64 {
        let minute = now.div_euclid(60_000) * 60_000;
        (0..=2880)
            .map(|offset| minute - offset * 60_000)
            .find(|at| self.active_window(*at - 1) != Some(active))
            .unwrap_or(minute)
    }

    pub fn next_check(&self, last: Option<i64>, now: i64) -> i64 {
        let Some(last) = last else {
            return now;
        };
        let regular = last.saturating_add(self.interval_seconds as i64 * 1000);
        let due = match self.active_window(now) {
            Some(index) => {
                let start = self.segment_start(now, index);
                if last < start {
                    return start; // Wake once when overdue; do not backfill missed slots.
                }
                let step = self.windows[index].interval_seconds.max(1) as i64 * 1000;
                start + ((last - start) / step + 1) * step
            }
            None => regular,
        };
        if due <= now {
            return due;
        }
        let Some(transition) = self.next_transition(now) else {
            return due;
        };
        if due < transition {
            return due;
        }
        if self.active_window(transition).is_some() {
            return transition; // Every window starts with its own first check.
        }
        // On exiting a window, resume the default interval from the last check.
        // If that interval already elapsed, wait until the window actually ends.
        let outside_due = regular.max(transition);
        self.next_transition(transition)
            .map(|entry| outside_due.min(entry))
            .unwrap_or(outside_due)
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Inventory {
    pub store: String,
    pub store_id: String,
    pub city: String,
    pub distance: String,
    pub product: String,
    pub part_number: String,
    pub status: String,
    pub pickup_quote: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Snapshot {
    #[serde(default)]
    pub next_check_at: Option<i64>,
    #[serde(default = "legacy_source")]
    pub source: Source,
    pub id: i64,
    pub observed_at: i64,
    pub source_at: Option<i64>,
    pub url: String,
    pub error: Option<String>,
    pub inventory: Vec<Inventory>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct HistoryPoint {
    pub next_check_at: Option<i64>,
    pub source: Source,
    pub id: i64,
    pub observed_at: i64,
    pub source_at: Option<i64>,
    pub error: Option<String>,
    pub total: i64,
    pub available: i64,
}

#[derive(Serialize, Debug)]
pub struct LastAvailable {
    pub snapshot_id: i64,
    pub observed_at: i64,
    pub inventory: Inventory,
}

pub fn newly_available(previous: Option<&Snapshot>, current: &Snapshot) -> Vec<Inventory> {
    if current.error.is_some() {
        return vec![];
    }
    let was_available: std::collections::HashSet<_> = previous
        .filter(|snapshot| snapshot.url == current.url && snapshot.error.is_none())
        .into_iter()
        .flat_map(|snapshot| snapshot.inventory.iter())
        .filter(|row| row.status == "available")
        .map(|row| (row.store_id.as_str(), row.part_number.as_str()))
        .collect();
    current
        .inventory
        .iter()
        .filter(|row| {
            row.status == "available"
                && !was_available.contains(&(row.store_id.as_str(), row.part_number.as_str()))
        })
        .cloned()
        .collect()
}

fn string(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .into()
}

pub fn parse_inventory(html: &str) -> Result<(Option<i64>, Vec<Inventory>)> {
    let document = scraper::Html::parse_document(html);
    let selector = scraper::Selector::parse("script#__NEXT_DATA__").map_err(|e| e.to_string())?;
    let script = document
        .select(&selector)
        .next()
        .ok_or("The source page has no inventory JSON; its structure may have changed.")?;
    let data: Value =
        serde_json::from_str(&script.text().collect::<String>()).map_err(|e| e.to_string())?;
    let props = &data["props"]["pageProps"];
    if props["status"].as_str() != Some("200") && props["status"].as_i64() != Some(200) {
        return Err(format!(
            "The source returned an inventory error: {}",
            props["error"]
        ));
    }
    let stores = props["stores"]
        .as_array()
        .ok_or("The source returned no store list.")?;
    let rows = parse_stores(stores)?;
    Ok((props["ts"].as_i64(), rows))
}

fn parse_stores(stores: &[Value]) -> Result<Vec<Inventory>> {
    let mut rows = Vec::new();
    for store in stores {
        let parts = store["partsAvailability"]
            .as_object()
            .ok_or("A store is missing inventory data.")?;
        for (part, availability) in parts {
            let regular = &availability["messageTypes"]["regular"];
            let product = string(regular, "storePickupProductTitle");
            let status = string(availability, "pickupDisplay");
            rows.push(Inventory {
                store: string(store, "storeName"),
                store_id: string(store, "storeNumber"),
                city: string(store, "city"),
                distance: string(store, "storeDistanceWithUnit"),
                product: if product.is_empty() {
                    part.clone()
                } else {
                    product
                },
                part_number: part.clone(),
                status: if status.is_empty() {
                    "unknown".into()
                } else {
                    status
                },
                pickup_quote: string(availability, "pickupSearchQuote"),
            });
        }
    }
    if rows.is_empty() {
        return Err("The source returned an empty inventory snapshot.".into());
    }
    Ok(rows)
}

pub fn apple_query(target: &crate::catalog::ResolvedTarget) -> Result<reqwest::Url> {
    target.target.validate(false)?;
    let mut endpoint = reqwest::Url::parse("https://www.apple.com/shop/retail/pickup-message")
        .map_err(|e| e.to_string())?;
    {
        let mut query = endpoint.query_pairs_mut();
        query
            .append_pair("fae", "true")
            .append_pair("pl", "true")
            .append_pair("mts.0", "regular")
            .append_pair("location", &target.target.zip_code);
        if target.category == "iphone" && target.target.carrier == "unlocked" {
            query.append_pair("cppart", "UNLOCKED/US");
        }
        if target.variants.is_empty() {
            return Err("Select a configuration from the Apple catalog.".into());
        }
        for (index, variant) in target.variants.iter().enumerate() {
            if !crate::catalog::valid_sku(&variant.part_number) {
                return Err("Catalog contains an invalid pickup part number.".into());
            }
            query.append_pair(&format!("parts.{index}"), &variant.part_number);
        }
    }
    Ok(endpoint)
}

pub fn apple_fulfillment_query(target: &crate::catalog::ResolvedTarget) -> Result<reqwest::Url> {
    let mut endpoint = apple_query(target)?;
    endpoint.set_path("/shop/fulfillment-messages");
    Ok(endpoint)
}

pub fn parse_apple_inventory(body: &str) -> Result<(Option<i64>, Vec<Inventory>)> {
    let data: Value = serde_json::from_str(body)
        .map_err(|_| "Apple inventory returned an unexpected response.".to_string())?;
    if data["head"]["status"].as_str() != Some("200")
        && data["head"]["status"].as_i64() != Some(200)
    {
        return Err(format!(
            "Apple inventory returned status {}: {}",
            data["head"]["status"], data["body"]["error"]
        ));
    }
    let stores = data
        .pointer("/body/content/pickupMessage/stores")
        .or_else(|| data.pointer("/body/stores"))
        .and_then(Value::as_array)
        .ok_or("Apple inventory returned no store list.")?;
    // Retail-store metadata timestamps do not identify when inventory changed.
    // Keep the collection time separately; Apple supplies no inventory update timestamp.
    Ok((
        None,
        parse_stores(stores).map_err(|e| format!("Apple inventory response failed: {e}"))?,
    ))
}

pub fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS settings (id INTEGER PRIMARY KEY CHECK(id=1), value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS snapshots (
            id INTEGER PRIMARY KEY, observed_at INTEGER NOT NULL, source_at INTEGER, url TEXT NOT NULL, error TEXT);
        CREATE INDEX IF NOT EXISTS snapshots_range ON snapshots(url, observed_at);
        CREATE TABLE IF NOT EXISTS inventory (
            snapshot_id INTEGER NOT NULL REFERENCES snapshots(id), store TEXT NOT NULL, store_id TEXT NOT NULL,
            city TEXT NOT NULL, distance TEXT NOT NULL, product TEXT NOT NULL, part_number TEXT NOT NULL,
            status TEXT NOT NULL, pickup_quote TEXT NOT NULL,
            PRIMARY KEY(snapshot_id, store_id, part_number));
        CREATE INDEX IF NOT EXISTS inventory_filter ON inventory(snapshot_id, store_id, product);")
        .map_err(|e| e.to_string())?;
    db.execute(
        "INSERT OR IGNORE INTO settings(id,value) VALUES(1,?1)",
        [serde_json::to_string(&Settings::default()).map_err(|e| e.to_string())?],
    )
    .map_err(|e| e.to_string())?;
    let has_source: bool = db
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('snapshots') WHERE name='source'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(|e| e.to_string())?
        > 0;
    if !has_source {
        db.execute(
            "ALTER TABLE snapshots ADD COLUMN source TEXT NOT NULL DEFAULT 'checker'",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    let has_plan = db
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('snapshots') WHERE name='next_check_at'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(|e| e.to_string())?
        > 0;
    if !has_plan {
        db.execute("ALTER TABLE snapshots ADD COLUMN next_check_at INTEGER", [])
            .map_err(|e| e.to_string())?;
    }
    let migration = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let identities: Vec<String> = {
        let mut statement = db
            .prepare("SELECT DISTINCT url FROM snapshots")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    for identity in identities {
        if let Ok(target) = crate::catalog::Target::from_key(&identity) {
            let key = target.key();
            if key != identity {
                db.execute(
                    "UPDATE snapshots SET url=?1 WHERE url=?2",
                    params![key, identity],
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }
    let settings = load_settings(db)?;
    db.execute(
        "UPDATE settings SET value=?1 WHERE id=1",
        [serde_json::to_string(&settings).map_err(|e| e.to_string())?],
    )
    .map_err(|e| e.to_string())?;
    migration.commit().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_settings(db: &Connection) -> Result<Settings> {
    let value: String = db
        .query_row("SELECT value FROM settings WHERE id=1", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&value).map_err(|e| e.to_string())
}

pub fn store_settings(db: &Connection, settings: &Settings) -> Result<()> {
    settings.validate()?;
    db.execute(
        "UPDATE settings SET value=?1 WHERE id=1",
        [serde_json::to_string(settings).map_err(|e| e.to_string())?],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn insert_snapshot(db: &mut Connection, snapshot: &Snapshot) -> Result<i64> {
    let tx = db.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO snapshots(observed_at,source_at,url,error,source,next_check_at) VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            snapshot.observed_at,
            snapshot.source_at,
            snapshot.url,
            snapshot.error,
            snapshot.source.as_str(), snapshot.next_check_at
        ],
    )
    .map_err(|e| e.to_string())?;
    let id = tx.last_insert_rowid();
    {
        let mut stmt = tx
            .prepare("INSERT INTO inventory VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)")
            .map_err(|e| e.to_string())?;
        for row in &snapshot.inventory {
            stmt.execute(params![
                id,
                row.store,
                row.store_id,
                row.city,
                row.distance,
                row.product,
                row.part_number,
                row.status,
                row.pickup_quote
            ])
            .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(id)
}

pub fn snapshot_by_id(db: &Connection, id: i64) -> Result<Snapshot> {
    let mut snapshot = db
        .query_row(
            "SELECT id, observed_at, source_at, url, error, source, next_check_at FROM snapshots WHERE id=?1",
            [id],
            |r| {
                Ok(Snapshot {
                    next_check_at: r.get(6)?,                    source: Source::from_db(r.get(5)?),
                    id: r.get(0)?,
                    observed_at: r.get(1)?,
                    source_at: r.get(2)?,
                    url: r.get(3)?,
                    error: r.get(4)?,
                    inventory: vec![],
                })
            },
        )
        .map_err(|e| e.to_string())?;
    let mut stmt = db
        .prepare(
            "SELECT store,store_id,city,distance,product,part_number,status,pickup_quote
        FROM inventory WHERE snapshot_id=?1 ORDER BY CAST(distance AS REAL), store, product",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([id], |r| {
            Ok(Inventory {
                store: r.get(0)?,
                store_id: r.get(1)?,
                city: r.get(2)?,
                distance: r.get(3)?,
                product: r.get(4)?,
                part_number: r.get(5)?,
                status: r.get(6)?,
                pickup_quote: r.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?;
    snapshot.inventory = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(snapshot)
}

pub fn latest_snapshot(db: &Connection, url: &str) -> Result<Option<Snapshot>> {
    let id: Option<i64> = db
        .query_row(
            "SELECT (SELECT id FROM snapshots WHERE url=?1 AND error IS NULL ORDER BY observed_at DESC,id DESC LIMIT 1)",
            [url],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    id.map(|id| snapshot_by_id(db, id)).transpose()
}

pub fn last_attempt(db: &Connection, url: &str) -> Result<Option<i64>> {
    db.query_row(
        "SELECT MAX(observed_at) FROM snapshots WHERE url=?1",
        [url],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

pub fn last_available(
    db: &Connection,
    url: &str,
    store_id: &str,
    product: &str,
) -> Result<Vec<LastAvailable>> {
    // Filter stores before ranking so a selected store keeps its own last sighting.
    // The latest successful availability observation for each configuration can
    // include multiple stores. Use collection time, including for imported history.
    let mut stmt = db.prepare(
        "WITH sightings AS (
            SELECT s.id, s.observed_at, i.*,
                DENSE_RANK() OVER (PARTITION BY i.product ORDER BY s.observed_at DESC, s.id DESC) AS rank
            FROM snapshots s JOIN inventory i ON i.snapshot_id=s.id
            WHERE s.url=?1 AND s.error IS NULL AND i.status='available'
                AND (?2='' OR i.store_id=?2) AND (?3='' OR i.product=?3)
        )
        SELECT id, observed_at, store, store_id, city, distance, product, part_number, status, pickup_quote
        FROM sightings WHERE rank=1 ORDER BY product, CAST(distance AS REAL), store"
    ).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![url, store_id, product], |r| {
            Ok(LastAvailable {
                snapshot_id: r.get(0)?,
                observed_at: r.get(1)?,
                inventory: Inventory {
                    store: r.get(2)?,
                    store_id: r.get(3)?,
                    city: r.get(4)?,
                    distance: r.get(5)?,
                    product: r.get(6)?,
                    part_number: r.get(7)?,
                    status: r.get(8)?,
                    pickup_quote: r.get(9)?,
                },
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

pub fn history(
    db: &Connection,
    url: &str,
    start: i64,
    end: i64,
    store_id: &str,
    product: &str,
) -> Result<Vec<HistoryPoint>> {
    if end < start {
        return Err("The end date must be after the start date.".into());
    }
    let mut stmt = db
        .prepare(
            "SELECT s.id, s.observed_at, s.source_at, s.error, COUNT(i.part_number),
        COALESCE(SUM(CASE WHEN i.status='available' THEN 1 ELSE 0 END),0), s.source, s.next_check_at
        FROM snapshots s LEFT JOIN inventory i ON i.snapshot_id=s.id
            AND (?4='' OR i.store_id=?4) AND (?5='' OR i.product=?5)
        WHERE s.url=?1 AND s.observed_at>=?2 AND s.observed_at<=?3
        GROUP BY s.id ORDER BY s.observed_at, s.id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![url, start, end, store_id, product], |r| {
            Ok(HistoryPoint {
                next_check_at: r.get(7)?,
                source: Source::from_db(r.get(6)?),
                id: r.get(0)?,
                observed_at: r.get(1)?,
                source_at: r.get(2)?,
                error: r.get(3)?,
                total: r.get(4)?,
                available: r.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

pub fn last_error(db: &Connection, url: &str) -> Result<Option<String>> {
    db.query_row(
        "SELECT (SELECT error FROM snapshots WHERE url=?1 ORDER BY observed_at DESC,id DESC LIMIT 1)",
        [url], |r| r.get(0),
    ).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: &str) -> i64 {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .timestamp_millis()
    }

    fn window(start_minute: u32, end_minute: u32, interval_seconds: u32) -> TimeWindow {
        TimeWindow {
            enabled: true,
            start_minute,
            end_minute,
            interval_seconds,
        }
    }

    #[test]
    fn multiple_windows_align_seconds_and_resume_default() {
        let settings = Settings {
            interval_seconds: 600,
            windows: vec![
                window(360, 660, 30),
                window(660, 720, 900),
                window(780, 840, 1),
            ],
            ..Settings::default()
        };
        settings.validate().unwrap();
        let before = ms("2026-10-28T05:59:00-07:00");
        let start = ms("2026-10-28T06:00:00-07:00");
        assert_eq!(settings.active_interval(before), 600);
        assert_eq!(settings.next_check(Some(before), before), start);
        assert_eq!(settings.active_interval(start), 30);
        assert_eq!(
            settings.next_check(Some(start + 1300), start + 1300),
            start + 30_000
        );
        assert_eq!(settings.next_check(None, start), start);
        let last = ms("2026-10-28T10:59:40-07:00");
        let adjacent = ms("2026-10-28T11:00:00-07:00");
        assert_eq!(settings.next_check(Some(last), last), adjacent);
        assert_eq!(settings.active_interval(adjacent), 900); // Slower window is supported.
        assert_eq!(
            settings.next_check(Some(adjacent + 500), adjacent + 500),
            adjacent + 900_000
        );
        let last = ms("2026-10-28T11:45:01-07:00");
        assert_eq!(
            settings.next_check(Some(last), last),
            ms("2026-10-28T12:00:00-07:00")
        );
        let one_second = ms("2026-10-28T13:00:00-07:00");
        assert_eq!(settings.active_interval(one_second), 1);
        assert_eq!(
            settings.next_check(Some(one_second + 4500), one_second + 4500),
            one_second + 5000
        );
        // A slow response skips slots already passed and never adds request duration to the grid.
        assert_eq!(
            settings.next_check(Some(start + 95_000), start + 95_000),
            start + 120_000
        );
        assert_eq!(
            settings.next_check(Some(before), ms("2026-10-28T08:00:00-07:00")),
            start
        );
    }

    #[test]
    fn window_exit_retains_existing_ten_minute_schedule() {
        let settings = Settings {
            interval_seconds: 600,
            windows: vec![window(360, 660, 120)],
            ..Settings::default()
        };
        let last = ms("2026-10-28T10:58:01-07:00");
        assert_eq!(
            settings.next_check(Some(last), last),
            ms("2026-10-28T11:08:01-07:00")
        );
        assert_eq!(
            settings.next_check(Some(last), ms("2026-10-28T11:00:00-07:00")),
            ms("2026-10-28T11:08:01-07:00")
        );
        assert_eq!(
            settings.active_interval(ms("2026-10-28T11:00:00-07:00")),
            600
        );
        let settings = Settings {
            interval_seconds: 86_400,
            windows: vec![window(360, 660, 86_400), window(720, 780, 30)],
            ..settings
        };
        let last = ms("2026-10-28T06:00:01-07:00");
        assert_eq!(
            settings.next_check(Some(last), last),
            ms("2026-10-28T12:00:00-07:00")
        );
    }

    #[test]
    fn overnight_windows_continue_across_midnight() {
        let settings = Settings {
            interval_seconds: 600,
            windows: vec![window(1380, 60, 30)],
            ..Settings::default()
        };
        settings.validate().unwrap();
        let before = ms("2026-10-28T22:59:00-07:00");
        assert_eq!(
            settings.next_check(Some(before), before),
            ms("2026-10-28T23:00:00-07:00")
        );
        let last = ms("2026-10-29T00:00:01-07:00");
        assert_eq!(settings.active_interval(last), 30);
        assert_eq!(
            settings.next_check(Some(last), last),
            ms("2026-10-29T00:00:30-07:00")
        );
        assert_eq!(
            settings.active_interval(ms("2026-10-29T01:00:00-07:00")),
            600
        );
    }

    #[test]
    fn pacific_windows_follow_dst_skips_and_repeated_hours() {
        let settings = Settings {
            interval_seconds: 86_400,
            windows: vec![window(360, 660, 120)],
            ..Settings::default()
        };
        for (before, start) in [
            ("2026-03-08T05:59:00-07:00", "2026-03-08T06:00:00-07:00"),
            ("2026-11-01T05:59:00-08:00", "2026-11-01T06:00:00-08:00"),
            ("2026-10-31T20:00:00-07:00", "2026-11-01T06:00:00-08:00"),
        ] {
            assert_eq!(settings.next_check(Some(ms(before)), ms(before)), ms(start));
        }
        let spring = Settings {
            windows: vec![window(150, 240, 30)],
            ..settings.clone()
        };
        let before = ms("2026-03-08T01:59:00-08:00");
        let start = ms("2026-03-08T03:00:00-07:00");
        assert_eq!(spring.next_check(Some(before), before), start);
        assert_eq!(
            spring.next_check(Some(start + 1000), start + 1000),
            start + 30_000
        );
        let fall = Settings {
            windows: vec![window(60, 90, 30)],
            ..settings
        };
        let first = ms("2026-11-01T01:00:00-07:00");
        let second = ms("2026-11-01T01:00:00-08:00");
        assert_eq!(fall.active_interval(first), 30);
        assert_eq!(fall.active_interval(second), 30);
        let between = ms("2026-11-01T01:45:00-07:00");
        assert_eq!(fall.active_interval(between), 86_400);
        assert_eq!(fall.next_check(Some(between), between), second);
        assert_eq!(fall.next_check(Some(first), second), second);
        assert_eq!(
            fall.next_check(Some(second + 1000), second + 1000),
            second + 30_000
        );
    }

    #[test]
    fn windows_validate_enabled_overlaps_and_second_limits() {
        let mut settings = Settings {
            windows: vec![window(360, 660, 1), window(660, 720, 86_400)],
            ..Settings::default()
        };
        settings.validate().unwrap();
        settings.windows[1].start_minute = 659;
        assert!(settings.validate().unwrap_err().contains("overlaps"));
        settings.windows[1].enabled = false;
        settings.validate().unwrap();
        settings.windows = vec![window(1380, 60, 30), window(30, 120, 20)];
        assert!(settings.validate().is_err());
        settings.windows[1].start_minute = 60;
        settings.validate().unwrap();
        for invalid in [
            window(60, 60, 30),
            window(0, 1440, 30),
            window(60, 120, 0),
            window(60, 120, 86_401),
        ] {
            settings.windows = vec![invalid];
            assert!(settings.validate().is_err());
        }
        settings.windows = vec![];
        settings.interval_seconds = 1;
        settings.source = Source::Checker;
        assert!(settings.validate().is_err());
        settings.source = Source::Apple;
        settings.validate().unwrap();
        let now = ms("2026-10-28T12:00:00-07:00");
        assert_eq!(settings.next_check(Some(now), now), now + 1000);
    }

    #[test]
    fn legacy_settings_migrate_without_changing_schedule() {
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let legacy = serde_json::json!({ "source": "apple", "url": DEFAULT_URL, "interval_minutes": 10,
            "enabled": true, "notify_available": true,
            "morning": {"enabled": true, "start_minute": 360, "end_minute": 660, "interval_minutes": 2} });
        db.execute(
            "UPDATE settings SET value=?1 WHERE id=1",
            [legacy.to_string()],
        )
        .unwrap();
        initialize(&db).unwrap();
        let settings = load_settings(&db).unwrap();
        assert_eq!(settings.source, Source::Apple);
        assert_eq!(settings.interval_seconds, 600);
        assert!(settings.enabled && settings.notify_available);
        assert_eq!(settings.windows.len(), 1);
        assert!(settings.windows[0].enabled);
        assert_eq!(settings.windows[0].start_minute, 360);
        assert_eq!(settings.windows[0].end_minute, 660);
        assert_eq!(settings.windows[0].interval_seconds, 120);
        let canonical: String = db
            .query_row("SELECT value FROM settings", [], |row| row.get(0))
            .unwrap();
        assert!(!canonical.contains("morning") && !canonical.contains("interval_minutes"));
        let mut mixed = legacy;
        mixed["windows"] = serde_json::json!([]);
        mixed["interval_seconds"] = serde_json::json!(30);
        let empty: Settings = serde_json::from_value(mixed.clone()).unwrap();
        assert!(empty.windows.is_empty());
        assert_eq!(empty.interval_seconds, 30);
        mixed.as_object_mut().unwrap().remove("windows");
        mixed["morning"]["enabled"] = serde_json::json!(false);
        let disabled: Settings = serde_json::from_value(mixed).unwrap();
        assert!(!disabled.windows[0].enabled);
        assert_eq!(disabled.windows[0].end_minute, 660);
        store_settings(&db, &settings).unwrap();
        assert_eq!(load_settings(&db).unwrap().windows[0].interval_seconds, 120);
    }

    #[test]
    fn apple_query_and_response_match_the_tracker() {
        let target = crate::catalog::builtin()
            .resolve(&crate::catalog::Target::from_key(DEFAULT_URL).unwrap())
            .unwrap();
        let url = apple_query(&target).unwrap();
        assert_eq!(url.host_str(), Some("www.apple.com"));
        assert_eq!(url.path(), "/shop/retail/pickup-message");
        let browser_url = apple_fulfillment_query(&target).unwrap();
        assert_eq!(browser_url.path(), "/shop/fulfillment-messages");
        assert_eq!(browser_url.query(), url.query());
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(pairs.get("location").unwrap(), "94025");
        assert_eq!(pairs.get("fae").unwrap(), "true");
        assert_eq!(pairs.get("pl").unwrap(), "true");
        assert_eq!(pairs.get("mts.0").unwrap(), "regular");
        assert_eq!(pairs.get("cppart").unwrap(), "UNLOCKED/US");
        assert_eq!(pairs.keys().filter(|k| k.starts_with("parts.")).count(), 8);
        let page: Value = serde_json::from_str(
            fixture("available")
                .split('>')
                .nth(2)
                .unwrap()
                .split('<')
                .next()
                .unwrap(),
        )
        .unwrap();
        let apple = serde_json::json!({"head":{"status":"200"}, "body":{"content":{"pickupMessage":{"stores":page["props"]["pageProps"]["stores"]}}}});
        let (ts, rows) = parse_apple_inventory(&apple.to_string()).unwrap();
        assert!(ts.is_none());
        assert_eq!(rows[0].status, "available");
        assert!(
            parse_apple_inventory(r#"{"head":{"status":"541"},"body":{"error":"Error"}}"#).is_err()
        );
        assert!(parse_apple_inventory("<html>Blocked</html>").is_err());
    }

    #[test]
    fn current_fulfillment_response_preserves_store_product_and_status_fields() {
        let fixture = include_str!("../tests/fixtures/apple-fulfillment.json");
        let (timestamp, rows) = parse_apple_inventory(fixture).unwrap();
        assert!(timestamp.is_none());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].store_id, "R002");
        assert_eq!(rows[0].store, "Palo Alto");
        assert_eq!(rows[0].product, "iPhone Duo 1TB Night Sky");
        assert_eq!(rows[0].part_number, "MK1L4LL/A");
        assert_eq!(rows[0].status, "unavailable");
        assert_eq!(rows[1].store_id, "R099");
        let mut data: Value = serde_json::from_str(fixture).unwrap();
        data["body"]["content"]["pickupMessage"]["stores"][0]["partsAvailability"]["MK1L4LL/A"]
            ["pickupDisplay"] = serde_json::json!("available");
        let (_, available) = parse_apple_inventory(&data.to_string()).unwrap();
        assert_eq!(available[0].status, "available");
        let old = serde_json::json!({"head":{"status":"200"}, "body":{"stores":data["body"]["content"]["pickupMessage"]["stores"]}});
        assert_eq!(parse_apple_inventory(&old.to_string()).unwrap().1.len(), 2);
        assert!(parse_apple_inventory(
            r#"{"head":{"status":"200"},"body":{"content":{"pickupMessage":{"stores":[]}}}}"#
        )
        .is_err());
        assert!(parse_apple_inventory(
            r#"{"head":{"status":"200"},"body":{"content":{"deliveryMessage":{}}}}"#
        )
        .is_err());
    }

    #[test]
    fn upgrades_history_and_legacy_settings_without_losing_data() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE snapshots(id INTEGER PRIMARY KEY,observed_at INTEGER NOT NULL,source_at INTEGER,url TEXT NOT NULL,error TEXT);
            INSERT INTO snapshots VALUES(1,1000,900,'legacy',NULL);
            CREATE TABLE settings(id INTEGER PRIMARY KEY,value TEXT NOT NULL);
            INSERT INTO settings VALUES(1,'{\"url\":\"https://iphonechecker.herokuapp.com/q/94025/iDuo/unlocked\",\"enabled\":true,\"interval_minutes\":10}');").unwrap();
        initialize(&db).unwrap();
        initialize(&db).unwrap();
        assert_eq!(load_settings(&db).unwrap().source, Source::Apple);
        assert_eq!(snapshot_by_id(&db, 1).unwrap().source, Source::Checker);
        let id = insert_snapshot(
            &mut db,
            &Snapshot {
                next_check_at: None,
                source: Source::Apple,
                id: 0,
                observed_at: 2000,
                source_at: None,
                url: "legacy".into(),
                error: None,
                inventory: vec![],
            },
        )
        .unwrap();
        assert_eq!(snapshot_by_id(&db, id).unwrap().source, Source::Apple);
        assert_eq!(
            history(&db, "legacy", 0, 3000, "", "").unwrap()[1].source,
            Source::Apple
        );
    }

    #[test]
    #[ignore = "Makes one request to Apple; requires available request budget"]
    fn live_source_has_parseable_inventory() {
        let target = crate::catalog::builtin()
            .resolve(&crate::catalog::Target::from_key(DEFAULT_URL).unwrap())
            .unwrap();
        let (result, meta) = crate::apple_http::fetch(&target);
        assert_eq!(meta.http_status, Some(200));
        let (ts, rows) = result.unwrap();
        assert!(ts.is_none());
        assert!(!rows.is_empty());
    }

    fn fixture(status: &str) -> String {
        format!(
            r#"<html><script id="__NEXT_DATA__" type="application/json">{{"props":{{"pageProps":{{"status":"200","ts":123,"stores":[{{"storeName":"Palo Alto","storeNumber":"R002","partsAvailability":{{"MK1E4LL/A":{{"pickupDisplay":"{status}","pickupSearchQuote":"Available Today","messageTypes":{{"regular":{{"storePickupProductTitle":"iPhone Duo 256GB Star White"}}}}}}}}}}]}}}}}}</script></html>"#
        )
    }

    #[test]
    fn extracts_product_status_and_rejects_missing_data() {
        let (ts, rows) = parse_inventory(&fixture("available")).unwrap();
        assert_eq!(ts, Some(123));
        assert_eq!(rows[0].product, "iPhone Duo 256GB Star White");
        assert_eq!(rows[0].status, "available");
        assert!(parse_inventory("<h1>Access denied</h1>").is_err());
        assert!(parse_inventory(r#"<script id="__NEXT_DATA__">{"props":{"pageProps":{"status":"429","error":"Rate limited"}}}</script>"#).is_err());
    }

    #[test]
    fn preserves_failures_and_filters_date_store_product() {
        let mut db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let (_, rows) = parse_inventory(&fixture("available")).unwrap();
        let id = insert_snapshot(
            &mut db,
            &Snapshot {
                next_check_at: None,
                source: Source::Checker,
                id: 0,
                observed_at: 1000,
                source_at: Some(123),
                url: DEFAULT_URL.into(),
                error: None,
                inventory: rows,
            },
        )
        .unwrap();
        insert_snapshot(
            &mut db,
            &Snapshot {
                next_check_at: None,
                source: Source::Checker,
                id: 0,
                observed_at: 2000,
                source_at: None,
                url: DEFAULT_URL.into(),
                error: Some("Network error".into()),
                inventory: vec![],
            },
        )
        .unwrap();
        let points = history(&db, DEFAULT_URL, 0, 3000, "R002", "").unwrap();
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].available, 1);
        assert!(points[1].error.is_some());
        assert_eq!(
            history(&db, DEFAULT_URL, 1500, 3000, "", "").unwrap().len(),
            1
        );
        assert_eq!(
            history(&db, DEFAULT_URL, 0, 3000, "R999", "").unwrap()[0].total,
            0
        );
        assert_eq!(
            history(&db, DEFAULT_URL, 0, 3000, "", "Other product").unwrap()[0].available,
            0
        );
        assert_eq!(latest_snapshot(&db, DEFAULT_URL).unwrap().unwrap().id, id);
        assert!(latest_snapshot(&db, "other").unwrap().is_none());
        assert_eq!(snapshot_by_id(&db, id).unwrap().inventory.len(), 1);
        assert_eq!(last_attempt(&db, DEFAULT_URL).unwrap(), Some(2000));
        assert_eq!(
            last_error(&db, DEFAULT_URL).unwrap().as_deref(),
            Some("Network error")
        );
        assert!(last_error(&db, "other").unwrap().is_none());
        let (_, older_rows) = parse_inventory(&fixture("unavailable")).unwrap();
        insert_snapshot(
            &mut db,
            &Snapshot {
                next_check_at: None,
                source: Source::Checker,
                id: 0,
                observed_at: 500,
                source_at: Some(100),
                url: DEFAULT_URL.into(),
                error: None,
                inventory: older_rows,
            },
        )
        .unwrap();
        assert_eq!(
            latest_snapshot(&db, DEFAULT_URL).unwrap().unwrap().id,
            id,
            "Importing an older observation must not replace the latest inventory"
        );
        assert_eq!(
            last_error(&db, DEFAULT_URL).unwrap().as_deref(),
            Some("Network error")
        );
        let (_, recovered_rows) = parse_inventory(&fixture("available")).unwrap();
        insert_snapshot(
            &mut db,
            &Snapshot {
                next_check_at: None,
                source: Source::Checker,
                id: 0,
                observed_at: 2500,
                source_at: Some(2400),
                url: DEFAULT_URL.into(),
                error: None,
                inventory: recovered_rows,
            },
        )
        .unwrap();
        assert!(
            last_error(&db, DEFAULT_URL).unwrap().is_none(),
            "A successful check must clear the current failure"
        );
    }

    #[test]
    fn last_available_preserves_configurations_and_filters_before_ranking() {
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        db.execute_batch(
            "INSERT INTO snapshots(id,observed_at,url,error) VALUES
                (1,1000,'tracker',NULL), (2,2000,'tracker',NULL),
                (3,3000,'tracker',NULL), (4,4000,'tracker','Network error'),
                (5,5000,'other',NULL), (6,500,'tracker',NULL),
                (7,2000,'tracker',NULL);",
        )
        .unwrap();
        let row = |snapshot, store: &str, product: &str, status: &str, distance: &str| {
            db.execute(
                "INSERT INTO inventory VALUES(?1,?2,?2,'City',?3,?4,?4,?5,'Available Today')",
                params![snapshot, store, distance, product, status],
            )
            .unwrap();
        };
        let a = "iPhone Duo 1TB Night Sky";
        let b = "iPhone Duo 256GB Star White";
        row(1, "R001", a, "available", "1 mi");
        row(1, "R002", a, "available", "2 mi");
        row(2, "R002", a, "available", "2 mi");
        row(2, "R003", a, "available", "3 mi");
        row(2, "R001", b, "available", "1 mi");
        row(3, "R002", a, "unavailable", "2 mi");
        row(3, "R001", b, "unavailable", "1 mi");
        row(3, "R001", "Never available", "unavailable", "1 mi");
        row(4, "R001", a, "available", "1 mi");
        row(5, "R001", a, "available", "1 mi");
        row(6, "R001", a, "available", "1 mi");
        row(7, "R005", a, "available", "12 mi");
        row(7, "R004", a, "available", "4 mi");

        let sightings = last_available(&db, "tracker", "", "").unwrap();
        assert_eq!(sightings.len(), 3);
        assert_eq!(sightings[0].snapshot_id, 7);
        assert_eq!(sightings[0].observed_at, 2000);
        assert_eq!(sightings[0].inventory.store_id, "R004");
        assert_eq!(sightings[1].inventory.store_id, "R005");
        assert_eq!(sightings[2].inventory.product, b);
        assert_eq!(sightings[2].snapshot_id, 2);

        let local = last_available(&db, "tracker", "R001", "").unwrap();
        assert_eq!(local.len(), 2);
        assert_eq!(local[0].snapshot_id, 1);
        assert_eq!(local[1].snapshot_id, 2);
        let model = last_available(&db, "tracker", "R002", a).unwrap();
        assert_eq!(model.len(), 1);
        assert_eq!(model[0].snapshot_id, 2);
        assert!(last_available(&db, "tracker", "", "Never available")
            .unwrap()
            .is_empty());
        assert!(last_available(&db, "tracker", "R999", "")
            .unwrap()
            .is_empty());
        assert_eq!(
            last_available(&db, "other", "", "").unwrap()[0].snapshot_id,
            5
        );
    }

    #[test]
    fn notifications_only_report_new_store_and_part_availability() {
        let mut db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        assert!(!load_settings(&db).unwrap().notify_available);
        let mut settings = load_settings(&db).unwrap();
        settings.notify_available = true;
        store_settings(&db, &settings).unwrap();
        assert!(load_settings(&db).unwrap().notify_available);

        let (_, fixture_rows) = parse_inventory(&fixture("available")).unwrap();
        let first = fixture_rows[0].clone();
        let mut second = first.clone();
        second.store_id = "R003".into();
        second.store = "Another store".into();
        let make = |observed_at, inventory, error| Snapshot {
            next_check_at: None,
            source: Source::Apple,
            id: 0,
            observed_at,
            source_at: None,
            url: DEFAULT_URL.into(),
            error,
            inventory,
        };
        let available = make(1000, vec![first.clone(), second.clone()], None);
        assert_eq!(newly_available(None, &available).len(), 2);
        insert_snapshot(&mut db, &available).unwrap();

        let mut renamed = first.clone();
        renamed.product = "Updated display name".into();
        let unchanged = make(2000, vec![second.clone(), renamed], None);
        let saved = latest_snapshot(&db, DEFAULT_URL).unwrap();
        assert!(newly_available(saved.as_ref(), &unchanged).is_empty());

        let failure = make(2500, vec![], Some("Network error".into()));
        assert!(newly_available(Some(&available), &failure).is_empty());
        insert_snapshot(&mut db, &failure).unwrap();
        let after_failure = latest_snapshot(&db, DEFAULT_URL).unwrap();
        assert!(newly_available(after_failure.as_ref(), &unchanged).is_empty());

        let mut new_part = first.clone();
        new_part.part_number = "NEWPART".into();
        let added = make(3000, vec![first.clone(), second.clone(), new_part], None);
        let changes = newly_available(after_failure.as_ref(), &added);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].part_number, "NEWPART");

        let mut absent = first.clone();
        absent.status = "unavailable".into();
        let unavailable = make(4000, vec![absent, second.clone()], None);
        insert_snapshot(&mut db, &unavailable).unwrap();
        let previous = latest_snapshot(&db, DEFAULT_URL).unwrap();
        let restored = make(5000, vec![first, second], None);
        let restocks = newly_available(previous.as_ref(), &restored);
        assert_eq!(restocks.len(), 1);
        assert_eq!(restocks[0].store_id, "R002");

        // The saved baseline is enough after a restart; no in-memory notification state is needed.
        insert_snapshot(&mut db, &restored).unwrap();
        let restarted = latest_snapshot(&db, DEFAULT_URL).unwrap();
        assert!(newly_available(restarted.as_ref(), &restored).is_empty());
    }

    #[test]
    fn settings_persist_and_enforce_second_limits() {
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let settings = Settings {
            interval_seconds: 1800,
            enabled: false,
            ..Settings::default()
        };
        store_settings(&db, &settings).unwrap();
        assert_eq!(load_settings(&db).unwrap().interval_seconds, 1800);
        assert!(!load_settings(&db).unwrap().enabled);
        assert!(Settings {
            interval_seconds: 1,
            ..settings.clone()
        }
        .validate()
        .is_ok());
        assert!(Settings {
            source: Source::Apple,
            interval_seconds: 1,
            ..settings.clone()
        }
        .validate()
        .is_ok());
        assert!(Settings {
            interval_seconds: 0,
            ..settings.clone()
        }
        .validate()
        .is_err());
        assert!(Settings {
            url: "https://example.com/q/94025/iDuo/unlocked".into(),
            ..settings
        }
        .validate()
        .is_err());
    }
}
