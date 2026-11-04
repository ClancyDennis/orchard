import { useState } from "react";
import { Clock3, LoaderCircle, RefreshCw, TrendingUp } from "lucide-react";
import type { PatternInsights } from "./api";

const hourLabel = (hour: number) => `${String(hour).padStart(2, "0")}:00`;
const percent = (value: number) => value < 0.1 && value > 0 ? "<0.1%" : `${value.toFixed(1)}%`;
const duration = (seconds: number) => seconds < 60 ? `${Math.round(seconds)} seconds`
  : seconds === 60 ? "1 minute" : `${(seconds / 60).toFixed(1).replace(/\.0$/, "")} minutes`;
const frequency = (low: number, high: number) => `${Math.round(low)}–${Math.round(high)}%`;

export function PatternPanel({ insights, loading, error, onConfigure, onRefresh, timezone }: {
  timezone: string;
  insights: PatternInsights | null;
  loading: boolean;
  error: string;
  onConfigure: () => void;
  onRefresh: () => void;
}) {
  const [chosenHour, setChosenHour] = useState<number | null>(null);
  const selected = insights?.hours[chosenHour ?? insights.recommendations[0]?.start_hour ?? 0];
  const maximum = Math.max(0.1, ...(insights?.hours.map(hour => hour.availability_percent ?? 0) ?? []));
  const strongest = insights?.recommendations[0];
  return (
    <section className="panel patterns-panel" aria-label="Availability patterns">
      <div className="panel-header">
        <div>
          <h2>When to watch</h2>
          <p>Patterns in the selected date range, stores and configurations · {timezone}</p>
        </div>
        <button className="icon-button" aria-label="Refresh availability patterns" onClick={onRefresh} disabled={loading}>
          {loading ? <LoaderCircle size={17} className="spin" /> : <RefreshCw size={17} />}
        </button>
      </div>
      {error ? <p className="pattern-empty" role="alert">Patterns could not be loaded: {error}</p>
        : loading && !insights ? <p className="pattern-empty">Looking for availability patterns…</p>
        : !insights || !insights.successful_checks ? <p className="pattern-empty">Collect successful checks in this view to start finding useful watch times.</p>
        : <>
          <div className="pattern-summary">
            <TrendingUp size={20} />
            <div>
              <strong>{strongest ? `Watch ${hourLabel(strongest.start_hour)}–${hourLabel(strongest.end_hour)} most closely`
                : "No ranked watch window yet"}</strong>
              <p>{insights.observed_days < 7 ? "Early evidence" : "Historical evidence"} · {insights.observed_days} observed {insights.observed_days === 1 ? "date" : "dates"} · {(insights.coverage_seconds / 3600).toFixed(1)} hours covered · {insights.source === "apple" ? "Apple direct" : "Checker (cached)"}</p>
              {!strongest && <p>{insights.arrival_events ? "Sightings need more daily coverage before they can be ranked."
                : "No new availability detected between nearby successful checks. Quiet hours may still contain missed stock."}</p>}
            </div>
          </div>
          {!!insights.recommendations.length && <div className="watch-windows">
            {insights.recommendations.map((window, index) => (
              <article className="watch-window" key={window.start_hour}>
                <span className="watch-rank">{index === 0 ? "Top watch window" : "Also worth watching"}</span>
                <h3><Clock3 size={15} /> {hourLabel(window.start_hour)}–{hourLabel(window.end_hour)}</h3>
                <p>Pickup seen on <strong>{window.opportunity_days} of {window.covered_days} covered days</strong></p>
                <p>{window.arrival_events} new availability {window.arrival_events === 1 ? "sighting" : "sightings"} · ~{percent(window.availability_percent)} of covered time</p>
                {window.covered_days < 7 && <span className="pattern-caution">Few days observed; timing may change</span>}
              </article>
            ))}
          </div>}
          <div className="pattern-hours">
            <div className="pattern-hours-heading"><strong>Availability by hour</strong><span>Darker = more covered time with pickup · click an hour</span></div>
            <div className="hour-grid">
              {insights.hours.map(hour => {
                const rate = hour.availability_percent;
                const shade = rate && rate > 0 ? 0.15 + Math.sqrt(rate / maximum) * 0.6 : 0.04;
                return <button key={hour.hour} className={`hour-cell${selected?.hour === hour.hour ? " selected" : ""}${rate === null ? " no-coverage" : ""}`}
                  style={rate === null ? undefined : { backgroundColor: `rgba(84, 138, 104, ${shade})` }}
                  aria-label={`${hourLabel(hour.hour)}–${hourLabel(hour.hour + 1)} ${timezone}: ${rate === null ? "no covered time" : `${percent(rate)} of covered time with pickup`}`}
                  aria-pressed={selected?.hour === hour.hour} onClick={() => setChosenHour(hour.hour)}>
                  <span>{String(hour.hour).padStart(2, "0")}</span>
                  <small>{rate === null ? "—" : rate > 0 ? "●" : "·"}</small>
                </button>;
              })}
            </div>
            {selected && <div className="hour-evidence" role="status">
              <strong>{hourLabel(selected.hour)}–{hourLabel(selected.hour + 1)}</strong>
              <span>{selected.availability_percent === null ? "No time coverage yet" : `~${percent(selected.availability_percent)} of covered time with pickup`}</span>
              <span>{(selected.coverage_seconds / 3600).toFixed(1)} hours covered · {selected.checks} checks · {selected.arrival_events} new sightings</span>
              <span>{selected.opportunity_days} / {selected.covered_days} covered days with pickup{selected.failed_checks ? ` · ${selected.failed_checks} failed checks excluded` : ""}</span>
              {selected.day_frequency_low !== null && selected.day_frequency_high !== null &&
                <span>Observed-day frequency range: {frequency(selected.day_frequency_low, selected.day_frequency_high)} (95% interval)</span>}
            </div>}
          </div>
          {!!insights.completed_bursts && <div className="burst-insight">
            <strong>Brief pickup windows</strong>
            <p>{insights.single_check_bursts} of {insights.completed_bursts} completed pickup bursts appeared on a single check.
              {insights.suggested_interval_seconds !== null && strongest ? ` Try ${duration(Math.max(insights.source === "apple" ? 60 : 1, insights.suggested_interval_seconds))} between checks during the highlighted windows.` : ""}</p>
            {insights.suggested_interval_seconds !== null && strongest && <button className="secondary-button" onClick={onConfigure}>Configure checks</button>}
          </div>}
          <details className="pattern-method">
            <summary>How these patterns are calculated</summary>
            <p>Availability is estimated by elapsed time between nearby successful checks, rather than by check counts. Each hour needs at least 30 minutes of coverage on a date for that date to count toward ranking. Adjacent hours with evidence are combined. Ranking uses the lower end of a 95% Wilson interval on days with pickup, then the fraction of covered time with pickup. Many checks on one day still count as one day.</p>
            <p>A new sighting is one check where at least one store/configuration changed from unavailable to available. The actual change happened between checks. First observations, source changes, failures, missing inventory and long gaps cannot establish a new arrival. Gaps over 30 minutes, or 1.8 times the planned interval, contribute no coverage.</p>
            {!!insights.completed_bursts && <p>A burst is a continuous run with at least one matching pickup option, bounded by successful unavailable checks. Open bursts, outages and range boundaries are excluded from duration estimates. The median duration is bounded by {duration(insights.median_duration_low_seconds ?? 0)} to {duration(insights.median_duration_high_seconds ?? 0)}; a single-check burst has no positive minimum duration.</p>}
            <p>Faster-rate suggestions use one quarter of the polling interval around brief bursts, limited to 1–120 seconds. Apple suggestions respect the one-minute pacing limit. This is a trial rate to gather more detail. Results update every minute; your schedule changes only when you save settings. {insights.failed_checks} failed checks and {insights.excluded_checks} checks with missing or unknown matching inventory are excluded.</p>
            <p>These are historical watch hints, not Apple's update schedule. Few observed dates give wide uncertainty, and polling can miss short availability between checks.{insights.source === "checker" ? " Checker caching also limits timing precision." : ""}</p>
          </details>
          <div className="chart-footer"><span>Local statistical analysis · no LLM call</span><span>Updated {new Date(insights.computed_at_ms).toLocaleTimeString([], {hour: "numeric", minute: "2-digit"})}</span></div>
        </>}
    </section>
  );
}
