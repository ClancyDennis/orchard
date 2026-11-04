import { invoke, isTauri } from "@tauri-apps/api/core";
import bundledCatalog from "../crates/orchard-core/data/catalog.json";

export interface Target { device_id: string; zip_code: string; part_number: string; carrier: string; }
export interface Variant { part_number: string; title: string; carrier: string; dimensions: Record<string,string>; standard_url: string; education_url: string; coming_soon: boolean; details_error?:string|null; }
export interface Device { id: string; name: string; category: string; buy_path: string; variants: Variant[]; checked_at_ms: number | null; last_error: string | null; }
export interface Catalog { schema_version: number; devices: Device[]; }
export function trackerKey(target: Target) {
  const url=new URL(`apple://us/${encodeURIComponent(target.zip_code)}/${encodeURIComponent(target.device_id)}/${encodeURIComponent(target.carrier)}`);
  if(target.part_number)url.searchParams.set("part",target.part_number);
  return url.toString();
}

export interface TimeWindow {
  enabled: boolean;
  start_minute: number;
  end_minute: number;
  interval_seconds: number;
}
export interface Settings {
  target: Target;
  connection: "auto" | "direct" | "browser";
  timezone: string;
  windows: TimeWindow[];
  source: "apple";
  url: string;
  interval_seconds: number;
  enabled: boolean;
  notify_available: boolean;
}
export interface Status {
  configured_interval_seconds: number;
  apple_request_after: number;
  apple_request_limit_reason: string | null;
  apple_failures: number;
  apple_session_open: boolean;
  server_retry_until: number;
  active_interval_seconds: number;
  settings: Settings;
  busy: boolean;
  next_due: number | null;
  last_attempt: number | null;
  last_error: string | null;
  db_path: string;
}
export interface CookieMetadata {
  name: string;
  domain: string;
  expires_at_ms: number | null;
  session: boolean;
  secure: boolean;
  http_only: boolean;
}
export interface ConnectionSample {
  page: { in_storefront: boolean; ready: string | null; visibility: string | null; bootstrap: boolean };
  cookies: CookieMetadata[];
  cookies_available: boolean;
}
export interface ConnectionEvent {
  at_ms: number;
  event: string;
  check_id: string | null;
  snapshot_id: number | null;
  next_check_at_ms: number | null;
  profile_id: string | null;
  profile_created_at_ms: number | null;
  browser_started_at_ms: number | null;
  page_loaded_at_ms: number | null;
  elapsed_ms: number | null;
  operation: string | null;
  success: boolean | null;
  error_kind: string | null;
  inventory_rows: number | null;
  response: { endpoint?: string | null; transport?: string | null; http_status: number | null; content_type: string | null; body_kind: string | null; body_bytes: number | null; retry_until_ms: number | null; server_date_ms: number | null };
  before: ConnectionSample | null;
  after: ConnectionSample | null;
  expired_since_previous_sample: string[];
}
export interface ConnectionReport {
  latest: ConnectionEvent | null;
  recent: ConnectionEvent[];
  last_success_at_ms: number | null;
  logging_available: boolean;
}
export interface Inventory {
  store: string;
  store_id: string;
  city: string;
  distance: string;
  product: string;
  part_number: string;
  status: string;
  pickup_quote: string;
}
export interface Snapshot {
  next_check_at: number | null;
  source: "apple" | "checker";
  id: number;
  observed_at: number;
  source_at: number | null;
  url: string;
  error: string | null;
  inventory: Inventory[];
}
export interface Point {
  next_check_at: number | null;
  source: "apple" | "checker";
  id: number;
  observed_at: number;
  source_at: number | null;
  error: string | null;
  total: number;
  available: number;
}
export interface LastAvailable {
  snapshot_id: number;
  observed_at: number;
  inventory: Inventory;
}
export interface HourPattern {
  hour: number;
  coverage_seconds: number;
  available_seconds: number;
  availability_percent: number | null;
  checks: number;
  failed_checks: number;
  arrival_events: number;
  covered_days: number;
  opportunity_days: number;
  day_frequency_low: number | null;
  day_frequency_high: number | null;
}
export interface WatchWindow {
  start_hour: number;
  end_hour: number;
  arrival_events: number;
  covered_days: number;
  opportunity_days: number;
  availability_percent: number;
  day_frequency_low: number;
  day_frequency_high: number;
}
export interface PatternInsights {
  computed_at_ms: number;
  source: "apple" | "checker";
  start_ms: number;
  end_ms: number;
  successful_checks: number;
  failed_checks: number;
  excluded_checks: number;
  observed_days: number;
  coverage_seconds: number;
  arrival_events: number;
  completed_bursts: number;
  single_check_bursts: number;
  median_duration_low_seconds: number | null;
  median_duration_high_seconds: number | null;
  suggested_interval_seconds: number | null;
  hours: HourPattern[];
  recommendations: WatchWindow[];
}
export const native = isTauri();

// A browser preview uses one real captured snapshot. Scheduling and storage live in Rust.
let preview: Promise<Snapshot> | undefined;
function previewSnapshot() {
  preview ??= fetch("/preview.json").then(async (response) => {
    if (!response.ok)
      throw new Error("Run this app with npm run desktop to start collecting.");
    const data = await response.json();
    return { ...data, id:1, next_check_at:null    } as Snapshot;
  });
  return preview;
}

export async function call<T>(
  command: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  if (native) return invoke<T>(command, args);
  if (command === "get_catalog") return bundledCatalog as T;
  const snapshot = await previewSnapshot();
  if (command === "get_latest" || command === "get_snapshot")
    return snapshot as T;
  if (command === "get_last_available")
    return snapshot.inventory
      .filter(
        (row) =>
          row.status === "available" &&
          (!args.storeId || row.store_id === args.storeId) &&
          (!args.product || row.product === args.product),
      )
      .map((inventory) => ({
        snapshot_id: snapshot.id,
        observed_at: snapshot.observed_at,
        inventory,
      })) as T;
  if (command === "get_status")
    return {
      settings: {
        target: { device_id:"ipad-pro",zip_code:"94025",part_number:"MDWU4LL/A",carrier:"unlocked" },
        connection:"auto",timezone:"America/Los_Angeles",
        windows: [],
        source: "apple",
        url: snapshot.url,
        interval_seconds: 900,
        enabled: false,
        notify_available: false,
      },
      busy: false,
      apple_failures: 0,
      apple_session_open: false,
      configured_interval_seconds: 900,
      apple_request_after: 0,
      apple_request_limit_reason: null,
      server_retry_until: 0,
      active_interval_seconds: 900,
      next_due: null,
      last_attempt: snapshot.observed_at,
      last_error: null,
      db_path: "Desktop app · local SQLite database",
    } as T;
  if (command === "get_patterns")
    return {
      source: "apple", computed_at_ms: Date.now(), start_ms: Number(args.startMs), end_ms: Number(args.endMs),
      successful_checks: 0, failed_checks: 0, excluded_checks: 0, observed_days: 0,
      coverage_seconds: 0, arrival_events: 0, completed_bursts: 0, single_check_bursts: 0,
      median_duration_low_seconds: null, median_duration_high_seconds: null,
      suggested_interval_seconds: null,
      hours: Array.from({ length: 24 }, (_, hour) => ({ hour, coverage_seconds: 0, available_seconds: 0,
        availability_percent: null, checks: 0, failed_checks: 0, arrival_events: 0, covered_days: 0,
        opportunity_days: 0, day_frequency_low: null, day_frequency_high: null })),
      recommendations: [],
    } as T;
  if (command === "get_history") {
    const inventory = snapshot.inventory.filter(
      (r) =>
        (!args.storeId || r.store_id === args.storeId) &&
        (!args.product || r.product === args.product),
    );
    return (
      snapshot.observed_at >= Number(args.startMs) &&
      snapshot.observed_at <= Number(args.endMs)
        ? [
            {
              next_check_at: null,
              source: "apple",
              id: 1,
              observed_at: snapshot.observed_at,
              source_at: snapshot.source_at,
              error: null,
              total: inventory.length,
              available: inventory.filter((r) => r.status === "available")
                .length,
            },
          ]
        : []
    ) as T;
  }
  throw new Error("Open the desktop app to run checks or change the schedule.");
}
