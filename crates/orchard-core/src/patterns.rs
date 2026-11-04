use crate::core::{now_ms, Result, Settings, Source};
use chrono::{DateTime, Timelike, Utc};
use chrono_tz::America::Los_Angeles;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
const HOUR: i64 = 3_600_000;
const MAX_GAP: i64 = 30 * 60_000;
const DAY_COVERAGE: f64 = 30.0 * 60.0;

#[derive(Debug)]
pub struct Sample {
    id: i64,
    at: i64,
    due: Option<i64>,
    failed: bool,
    unknown: bool,
    inventory: BTreeMap<(String, String), bool>,
}
impl Sample {
    fn valid(&self) -> bool {
        !self.failed && !self.unknown && !self.inventory.is_empty()
    }
    fn available(&self) -> bool {
        self.inventory.values().any(|available| *available)
    }
}

#[derive(Serialize, Debug)]
pub struct HourPattern {
    pub hour: usize,
    pub coverage_seconds: f64,
    pub available_seconds: f64,
    pub availability_percent: Option<f64>,
    pub checks: usize,
    pub failed_checks: usize,
    pub arrival_events: usize,
    pub covered_days: usize,
    pub opportunity_days: usize,
    pub day_frequency_low: Option<f64>,
    pub day_frequency_high: Option<f64>,
}
#[derive(Serialize, Debug)]
pub struct WatchWindow {
    pub start_hour: usize,
    pub end_hour: usize,
    pub arrival_events: usize,
    pub covered_days: usize,
    pub opportunity_days: usize,
    pub availability_percent: f64,
    pub day_frequency_low: f64,
    pub day_frequency_high: f64,
}
#[derive(Serialize, Debug)]
pub struct PatternInsights {
    pub source: Source,
    pub computed_at_ms: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub successful_checks: usize,
    pub failed_checks: usize,
    pub excluded_checks: usize,
    pub observed_days: usize,
    pub coverage_seconds: f64,
    pub arrival_events: usize,
    pub completed_bursts: usize,
    pub single_check_bursts: usize,
    pub median_duration_low_seconds: Option<f64>,
    pub median_duration_high_seconds: Option<f64>,
    pub suggested_interval_seconds: Option<u32>,
    pub hours: Vec<HourPattern>,
    pub recommendations: Vec<WatchWindow>,
}

// Read only the selected source. Cached Checker snapshots are never mixed into Apple evidence.
// One preceding observation establishes a baseline at the selected range's boundary.
pub fn read_samples(
    db: &Connection,
    settings: &Settings,
    start: i64,
    end: i64,
    store: &str,
    product: &str,
) -> Result<Vec<Sample>> {
    if end < start {
        return Err("The end date must be after the start date.".into());
    }
    let mut statement = db.prepare(
        "SELECT s.id,s.observed_at,s.next_check_at,s.error,i.store_id,i.part_number,i.status
         FROM snapshots s LEFT JOIN inventory i ON i.snapshot_id=s.id
           AND (?5='' OR i.store_id=?5) AND (?6='' OR i.product=?6)
         WHERE s.url=?1 AND s.source=?2 AND s.observed_at<=?4 AND
           (s.observed_at>=?3 OR s.id=(SELECT id FROM snapshots
             WHERE url=?1 AND source=?2 AND observed_at<?3 ORDER BY observed_at DESC,id DESC LIMIT 1))
         ORDER BY s.observed_at,s.id,i.store_id,i.part_number"
    ).map_err(|e| e.to_string())?;
    let mut rows = statement
        .query(params![
            settings.url,
            settings.source.as_str(),
            start,
            end,
            store,
            product
        ])
        .map_err(|e| e.to_string())?;
    let mut samples: Vec<Sample> = vec![];
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let id: i64 = row.get(0).map_err(|e| e.to_string())?;
        if samples.last().map(|sample| sample.id) != Some(id) {
            samples.push(Sample {
                id,
                at: row.get(1).map_err(|e| e.to_string())?,
                due: row.get(2).map_err(|e| e.to_string())?,
                failed: row
                    .get::<_, Option<String>>(3)
                    .map_err(|e| e.to_string())?
                    .is_some(),
                unknown: false,
                inventory: BTreeMap::new(),
            });
        }
        let key: Option<String> = row.get(4).map_err(|e| e.to_string())?;
        if let Some(store_id) = key {
            let part: String = row.get(5).map_err(|e| e.to_string())?;
            let status: String = row.get(6).map_err(|e| e.to_string())?;
            let sample = samples.last_mut().unwrap();
            sample.unknown |= status.is_empty() || status == "unknown";
            sample
                .inventory
                .insert((store_id, part), status == "available");
        }
    }
    Ok(samples)
}

#[derive(Default)]
struct DayEvidence {
    seconds: f64,
    opportunity: bool,
}
#[derive(Default)]
struct Evidence {
    seconds: f64,
    available_seconds: f64,
    checks: usize,
    failed: usize,
    arrivals: usize,
    days: BTreeMap<String, DayEvidence>,
}
fn clock(at: i64, timezone: chrono_tz::Tz) -> Option<(String, usize)> {
    let local = DateTime::<Utc>::from_timestamp_millis(at)?.with_timezone(&timezone);
    Some((local.date_naive().to_string(), local.hour() as usize))
}
fn continuous(previous: &Sample, current: &Sample, fallback_seconds: u32) -> bool {
    let gap = current.at - previous.at;
    let planned = previous
        .due
        .unwrap_or(previous.at + fallback_seconds as i64 * 1000)
        - previous.at;
    previous.valid()
        && current.valid()
        && gap > 0
        && gap <= MAX_GAP
        && gap as f64 <= planned.max(1000) as f64 * 1.8
        && previous.inventory.keys().eq(current.inventory.keys())
}
// Wilson intervals rank repeatability by observed days, not by correlated polling counts.
fn wilson(positive: usize, total: usize) -> Option<(f64, f64)> {
    if total == 0 {
        return None;
    }
    let n = total as f64;
    let p = positive as f64 / n;
    let z2 = 1.96_f64.powi(2);
    let center = (p + z2 / (2.0 * n)) / (1.0 + z2 / n);
    let half = 1.96 * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / (1.0 + z2 / n);
    Some((
        (center - half).max(0.0) * 100.0,
        (center + half).min(1.0) * 100.0,
    ))
}
fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len() % 2 == 1 {
        values[middle]
    } else {
        (values[middle - 1] + values[middle]) / 2.0
    })
}
struct Burst {
    before: i64,
    first: i64,
    last: i64,
    checks: usize,
    resolution: i64,
}

pub fn analyze(samples: &[Sample], settings: &Settings, start: i64, end: i64) -> PatternInsights {
    let timezone = settings
        .timezone
        .parse::<chrono_tz::Tz>()
        .unwrap_or(Los_Angeles);
    let mut evidence: Vec<Evidence> = (0..24).map(|_| Evidence::default()).collect();
    let mut dates = BTreeSet::new();
    let mut successful_checks = 0;
    let mut failed_checks = 0;
    let mut excluded_checks = 0;
    let mut arrival_events = 0;
    let mut burst: Option<Burst> = None;
    let mut lower = vec![];
    let mut upper = vec![];
    let mut resolutions = vec![];
    let mut single_check_bursts = 0;
    for (index, current) in samples.iter().enumerate() {
        let within = current.at >= start && current.at <= end;
        if within {
            if current.failed {
                failed_checks += 1;
            } else if !current.valid() {
                excluded_checks += 1;
            } else {
                successful_checks += 1;
            }
            if let Some((date, hour)) = clock(current.at, timezone) {
                if current.failed {
                    evidence[hour].failed += 1;
                }
                if current.valid() {
                    dates.insert(date.clone());
                    evidence[hour].checks += 1;
                    evidence[hour].days.entry(date).or_default().opportunity |= current.available();
                }
            }
        }
        let Some(previous) = index.checked_sub(1).and_then(|i| samples.get(i)) else {
            continue;
        };
        if !continuous(previous, current, settings.interval_seconds) {
            burst = None; // Failures, missing products and gaps censor rather than end a burst.
            continue;
        }
        // Estimate time in stock between adjacent successful checks; never extend into the future.
        let mut at = previous.at.max(start);
        let until = current.at.min(end);
        while at < until {
            let next = ((at.div_euclid(60_000) + 1) * 60_000).min(until);
            if let Some((date, hour)) = clock(at, timezone) {
                let seconds = (next - at) as f64 / 1000.0;
                let row = &mut evidence[hour];
                row.seconds += seconds;
                let day = row.days.entry(date.clone()).or_default();
                day.seconds += seconds;
                if previous.available() {
                    row.available_seconds += seconds;
                    day.opportunity = true;
                }
                dates.insert(date);
            }
            at = next;
        }
        if within
            && current
                .inventory
                .iter()
                .any(|(key, available)| *available && previous.inventory.get(key) == Some(&false))
        {
            if let Some((_, hour)) = clock(current.at, timezone) {
                evidence[hour].arrivals += 1;
                arrival_events += 1;
            }
        }
        if current.available() && !previous.available() && within && previous.at >= start {
            burst = Some(Burst {
                before: previous.at,
                first: current.at,
                last: current.at,
                checks: 1,
                resolution: current.at - previous.at,
            });
        } else if current.available() {
            if let Some(open) = burst.as_mut() {
                open.last = current.at;
                open.checks += 1;
                open.resolution = open.resolution.min(current.at - previous.at);
            }
        } else if let Some(open) = burst.take() {
            if within {
                lower.push((open.last - open.first) as f64 / 1000.0);
                upper.push((current.at - open.before) as f64 / 1000.0);
                resolutions.push(open.resolution.min(current.at - previous.at) as f64 / 1000.0);
                single_check_bursts += usize::from(open.checks == 1);
            }
        }
    }
    let hours: Vec<_> = evidence
        .iter()
        .enumerate()
        .map(|(hour, row)| {
            let days: Vec<_> = row
                .days
                .values()
                .filter(|day| day.seconds >= DAY_COVERAGE)
                .collect();
            let positive = days.iter().filter(|day| day.opportunity).count();
            let interval = wilson(positive, days.len());
            HourPattern {
                hour,
                coverage_seconds: row.seconds,
                available_seconds: row.available_seconds,
                availability_percent: (row.seconds > 0.0)
                    .then(|| row.available_seconds / row.seconds * 100.0),
                checks: row.checks,
                failed_checks: row.failed,
                arrival_events: row.arrivals,
                covered_days: days.len(),
                opportunity_days: positive,
                day_frequency_low: interval.map(|v| v.0),
                day_frequency_high: interval.map(|v| v.1),
            }
        })
        .collect();
    // Combine adjacent hours with evidence into readable daily windows.
    let mut recommendations: Vec<WatchWindow> = vec![];
    let mut hour = 0;
    while hour < 24 {
        if hours[hour].opportunity_days == 0 {
            hour += 1;
            continue;
        }
        let start_hour = hour;
        while hour < 24 && hours[hour].opportunity_days > 0 {
            hour += 1;
        }
        let mut covered = BTreeSet::new();
        let mut positive = BTreeSet::new();
        for row in &evidence[start_hour..hour] {
            for (date, day) in &row.days {
                if day.seconds >= DAY_COVERAGE {
                    covered.insert(date.clone());
                    if day.opportunity {
                        positive.insert(date.clone());
                    }
                }
            }
        }
        let seconds: f64 = evidence[start_hour..hour].iter().map(|r| r.seconds).sum();
        let available: f64 = evidence[start_hour..hour]
            .iter()
            .map(|r| r.available_seconds)
            .sum();
        let (low, high) = wilson(positive.len(), covered.len()).unwrap();
        recommendations.push(WatchWindow {
            start_hour,
            end_hour: hour,
            arrival_events: evidence[start_hour..hour].iter().map(|r| r.arrivals).sum(),
            covered_days: covered.len(),
            opportunity_days: positive.len(),
            availability_percent: available / seconds * 100.0,
            day_frequency_low: low,
            day_frequency_high: high,
        });
    }
    recommendations.sort_by(|a, b| {
        b.day_frequency_low
            .total_cmp(&a.day_frequency_low)
            .then(b.availability_percent.total_cmp(&a.availability_percent))
            .then(a.start_hour.cmp(&b.start_hour))
    });
    recommendations.truncate(3);
    let completed_bursts = lower.len();
    // A quarter of the observed polling resolution gives four chances per former check interval.
    // This is an experiment suggestion, not a measured stock lifetime or an automatic schedule change.
    let suggested_interval_seconds =
        if completed_bursts >= 3 && single_check_bursts * 2 >= completed_bursts {
            median(&mut resolutions).map(|resolution| {
                let rate = (resolution / 4.0).clamp(1.0, 120.0);
                if rate >= 5.0 {
                    (rate / 5.0).round() as u32 * 5
                } else {
                    rate.round().max(1.0) as u32
                }
            })
        } else {
            None
        };
    PatternInsights {
        source: settings.source,
        computed_at_ms: now_ms(),
        start_ms: start,
        end_ms: end,
        successful_checks,
        failed_checks,
        excluded_checks,
        observed_days: dates.len(),
        coverage_seconds: evidence.iter().map(|r| r.seconds).sum(),
        arrival_events,
        completed_bursts,
        single_check_bursts,
        median_duration_low_seconds: median(&mut lower),
        median_duration_high_seconds: median(&mut upper),
        suggested_interval_seconds,
        hours,
        recommendations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{initialize, insert_snapshot, Inventory, Snapshot, DEFAULT_URL};

    fn ms(value: &str) -> i64 {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .timestamp_millis()
    }
    fn sample(at: i64, available: bool, interval_seconds: u32) -> Sample {
        Sample {
            id: at,
            at,
            due: Some(at + interval_seconds as i64 * 1000),
            failed: false,
            unknown: false,
            inventory: BTreeMap::from([(("R001".into(), "A".into()), available)]),
        }
    }
    fn settings(seconds: u32) -> Settings {
        Settings {
            interval_seconds: seconds,
            target: crate::catalog::Target::from_key(DEFAULT_URL).unwrap(),
            url: DEFAULT_URL.into(),
            ..Settings::default()
        }
    }

    #[test]
    fn time_weighting_and_day_confidence_ignore_extra_identical_polls() {
        let start = ms("2026-10-27T08:00:00-07:00");
        let end = start + HOUR;
        let coarse = vec![
            sample(start, false, 1800),
            sample(start + HOUR / 2, true, 1800),
            sample(end, false, 1800),
        ];
        let dense: Vec<_> = (0..=60)
            .map(|minute| sample(start + minute * 60_000, (30..60).contains(&minute), 60))
            .collect();
        let a = analyze(&coarse, &settings(1800), start, end);
        let b = analyze(&dense, &settings(60), start, end);
        assert_eq!(a.hours[8].coverage_seconds, 3600.0);
        assert_eq!(a.hours[8].availability_percent, Some(50.0));
        assert_eq!(
            a.hours[8].availability_percent,
            b.hours[8].availability_percent
        );
        assert_eq!(a.hours[8].covered_days, 1);
        assert_eq!(a.hours[8].day_frequency_low, b.hours[8].day_frequency_low);
        assert_eq!(a.arrival_events, 1);
        assert_eq!(a.arrival_events, b.arrival_events);
        assert_eq!(a.recommendations[0].start_hour, 8);
        assert!(a.successful_checks < b.successful_checks);
    }

    #[test]
    fn failures_gaps_unknown_and_missing_configurations_are_not_unavailable() {
        let start = ms("2026-10-27T08:00:00-07:00");
        let mut failed = sample(start + 60_000, false, 60);
        failed.failed = true;
        let trace = vec![
            sample(start, false, 60),
            failed,
            sample(start + 120_000, true, 60),
            sample(start + 180_000, false, 60),
        ];
        let result = analyze(&trace, &settings(60), start, start + 240_000);
        assert_eq!(result.failed_checks, 1);
        assert_eq!(result.arrival_events, 0); // Availability after an outage has no known arrival time.
        assert_eq!(result.coverage_seconds, 60.0);
        assert_eq!(result.completed_bursts, 0); // The outage censors the burst's beginning.
        for kind in ["gap", "unknown", "missing"] {
            let mut next = sample(start + 60_000, true, 60);
            let mut previous = sample(start, false, 60);
            match kind {
                "gap" => next.at = start + HOUR,
                "unknown" => previous.unknown = true,
                _ => {
                    previous.inventory.clear();
                }
            }
            let result = analyze(&[previous, next], &settings(60), start, start + HOUR);
            assert_eq!(result.coverage_seconds, 0.0);
            assert_eq!(result.arrival_events, 0);
        }
        let mut missing = sample(start + 60_000, true, 60);
        missing.inventory.insert(("R002".into(), "B".into()), true);
        assert_eq!(
            analyze(
                &[sample(start, false, 60), missing],
                &settings(60),
                start,
                start + HOUR
            )
            .arrival_events,
            0
        );
    }

    #[test]
    fn range_clipping_uses_baseline_without_extending_last_observation() {
        let start = ms("2026-10-27T08:00:00-07:00");
        let trace = vec![
            sample(start, true, 600),
            sample(start + 600_000, true, 600),
            sample(start + 1_200_000, false, 600),
        ];
        let result = analyze(&trace, &settings(600), start + 300_000, start + 1_500_000);
        assert_eq!(result.coverage_seconds, 900.0);
        assert_eq!(result.hours[8].availability_percent, Some(100.0));
        assert_eq!(result.successful_checks, 2);
        assert_eq!(result.arrival_events, 0);
        assert_eq!(result.completed_bursts, 0); // Already available at the left boundary.
        assert!(result.recommendations.is_empty()); // Fewer than 30 minutes of coverage that day/hour.
    }

    #[test]
    fn overnight_and_repeated_pacific_hour_keep_real_time_and_unique_dates() {
        let before = ms("2026-10-27T23:50:00-07:00");
        let after = ms("2026-10-28T00:10:00-07:00");
        let midnight = analyze(
            &[sample(before, true, 1200), sample(after, false, 1200)],
            &settings(1200),
            before,
            after,
        );
        assert_eq!(midnight.hours[23].available_seconds, 600.0);
        assert_eq!(midnight.hours[0].available_seconds, 600.0);
        assert_eq!(midnight.observed_days, 2);
        let times = [
            "2026-11-01T01:00:00-07:00",
            "2026-11-01T01:30:00-07:00",
            "2026-11-01T01:59:00-07:00",
            "2026-11-01T01:01:00-08:00",
            "2026-11-01T01:31:00-08:00",
        ];
        let trace: Vec<_> = times
            .iter()
            .enumerate()
            .map(|(i, time)| sample(ms(time), i > 0 && i < 4, 1800))
            .collect();
        let repeated = analyze(&trace, &settings(1800), trace[0].at, trace[4].at);
        assert_eq!(repeated.observed_days, 1);
        assert_eq!(repeated.hours[1].covered_days, 1);
        assert_eq!(repeated.hours[1].coverage_seconds, 91.0 * 60.0);
        let spring = [
            sample(ms("2026-03-08T01:55:00-08:00"), true, 600),
            sample(ms("2026-03-08T03:05:00-07:00"), false, 600),
        ];
        let skipped = analyze(&spring, &settings(600), spring[0].at, spring[1].at);
        assert_eq!(skipped.hours[1].coverage_seconds, 300.0);
        assert_eq!(skipped.hours[2].coverage_seconds, 0.0);
        assert_eq!(skipped.hours[3].coverage_seconds, 300.0);
    }

    #[test]
    fn single_check_bursts_are_interval_censored_and_suggest_a_trial_rate() {
        let start = ms("2026-10-27T08:00:00-07:00");
        let trace: Vec<_> = (0..=9)
            .map(|i| sample(start + i * 120_000, i % 3 == 1, 120))
            .collect();
        let result = analyze(&trace, &settings(120), start, start + HOUR);
        assert_eq!(result.completed_bursts, 3);
        assert_eq!(result.single_check_bursts, 3);
        assert_eq!(result.median_duration_low_seconds, Some(0.0));
        assert_eq!(result.median_duration_high_seconds, Some(240.0));
        assert_eq!(result.suggested_interval_seconds, Some(30));
        // Never close a burst at an outage or at the end of the selected range.
        let mut outage = sample(start + 480_000, false, 120);
        outage.failed = true;
        let trace = [
            sample(start, false, 120),
            sample(start + 120_000, true, 120),
            outage,
        ];
        assert_eq!(
            analyze(&trace, &settings(120), start, start + HOUR).completed_bursts,
            0
        );
    }

    #[test]
    fn database_analysis_separates_sources_and_respects_filters_and_baseline() {
        let mut db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let start = ms("2026-10-27T08:00:00-07:00");
        let make = |at, source, status: &str, store: &str| Snapshot {
            id: 0,
            observed_at: at,
            source_at: None,
            next_check_at: Some(at + 600_000),
            source,
            url: DEFAULT_URL.into(),
            error: None,
            inventory: vec![Inventory {
                store: store.into(),
                store_id: store.into(),
                city: "Test".into(),
                distance: "1 mi".into(),
                product: "Model A".into(),
                part_number: "A".into(),
                status: status.into(),
                pickup_quote: "".into(),
            }],
        };
        insert_snapshot(&mut db, &make(start, Source::Apple, "unavailable", "R001")).unwrap();
        insert_snapshot(
            &mut db,
            &make(start + 300_000, Source::Checker, "available", "R001"),
        )
        .unwrap();
        insert_snapshot(
            &mut db,
            &make(start + 600_000, Source::Apple, "available", "R001"),
        )
        .unwrap();
        let chosen = settings(600);
        let samples = read_samples(
            &db,
            &chosen,
            start + 300_000,
            start + 600_000,
            "R001",
            "Model A",
        )
        .unwrap();
        assert_eq!(samples.len(), 2); // Apple baseline plus Apple observation; Checker is excluded.
        let result = analyze(&samples, &chosen, start + 300_000, start + 600_000);
        assert_eq!(result.successful_checks, 1);
        assert_eq!(result.arrival_events, 1);
        assert_eq!(result.coverage_seconds, 300.0);
        assert_eq!(result.completed_bursts, 0);
        let empty = read_samples(&db, &chosen, start, start + HOUR, "R002", "Model A").unwrap();
        assert_eq!(
            analyze(&empty, &chosen, start, start + HOUR).successful_checks,
            0
        );
        assert!(read_samples(&db, &chosen, start + HOUR, start, "", "").is_err());
    }

    #[test]
    fn adjacent_watch_hours_merge_without_counting_one_day_twice() {
        let mut trace = vec![];
        for date in ["2026-10-26", "2026-10-27", "2026-10-28"] {
            let start = ms(&format!("{date}T08:00:00-07:00"));
            for i in 0..=4 {
                trace.push(sample(start + i * HOUR / 2, i > 0 && i < 4, 1800));
            }
        }
        let result = analyze(
            &trace,
            &settings(1800),
            trace[0].at,
            trace.last().unwrap().at,
        );
        assert_eq!(result.recommendations.len(), 1);
        let best = &result.recommendations[0];
        assert_eq!((best.start_hour, best.end_hour), (8, 10));
        assert_eq!(best.covered_days, 3);
        assert_eq!(best.opportunity_days, 3);
        assert_eq!(best.arrival_events, 3);
        assert!((best.day_frequency_low - 43.849).abs() < 0.01);
    }
}
