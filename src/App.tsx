import { useCallback, useEffect, useMemo, useRef, useState, createContext, useContext } from "react";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  isPermissionGranted,
  requestPermission,
} from "@tauri-apps/plugin-notification";
import { purchaseLinks } from "./purchase";
import { PatternPanel } from "./PatternPanel";
import { ConnectionPanel } from "./ConnectionPanel";
import { DeviceSelector } from "./DeviceSelector";
import {
  Activity,
  ArrowDownToLine,
  ArrowUpRight,
  CalendarDays,
  Check,
  ChevronDown,
  Clock3,
  Database,
  LayoutDashboard,
  LoaderCircle,
  MapPin,
  Pause,
  Play,
  RefreshCw,
  Settings2,
  Smartphone,
  X,
} from "lucide-react";
import {
  Area,
  CartesianGrid,
  ComposedChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import {
  call,
  native,
  type Inventory,
  type LastAvailable,
  type Point,
  type PatternInsights,
  type Settings,
  type Snapshot,
  type Status,
  type TimeWindow,
  type Catalog,
  type HistoryView,
  type HistoryTarget,
} from "./api";

type Range = "day" | "week" | "month" | "custom";
type Page = "overview" | "log";
const DAY = 86_400_000;
const clockInput = (minute: number) =>
  `${String(Math.floor(minute / 60)).padStart(2, "0")}:${String(minute % 60).padStart(2, "0")}`;
const clockMinutes = (value: string) => {
  const [hour, minute] = value.split(":").map(Number);
  return hour * 60 + minute;
};
const dateTime = (ms: number) =>
  new Date(ms).toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
    second: "2-digit",
  });
const time = (ms: number) =>
  new Date(ms).toLocaleTimeString([], { hour: "numeric", minute: "2-digit", second: "2-digit" });
const localInput = (ms: number) => {
  const date = new Date(ms);
  return new Date(ms - date.getTimezoneOffset() * 60_000)
    .toISOString()
    .slice(0, 16);
};
const errorText = (error: unknown) =>
  error instanceof Error ? error.message : String(error);

function describeError(error: string): string {
  const prefix = "The source returned an inventory error: ";
  if (!error.startsWith(prefix)) return error;
  try {
    const details = JSON.parse(error.slice(prefix.length));
    return `The inventory website returned error ${details.code ?? "unknown"}. Your saved history is still available.`;
  } catch {
    return error;
  }
}

async function exportFile(
  name: string,
  content: string,
  type = "text/csv",
): Promise<string> {
  if (native) return call<string>("save_export", { name, content });
  const url = URL.createObjectURL(new Blob([content], { type }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
  return "Downloaded export";
}
const csv = (value: unknown) =>
  `"${String(value ?? "").replaceAll('"', '""')}"`;

const rateParts = (seconds: number) =>
  seconds % 60 === 0
    ? [seconds / 60, seconds === 60 ? "minute" : "minutes"] as const
    : [seconds, seconds === 1 ? "second" : "seconds"] as const;
const rateLabel = (seconds: number) => rateParts(seconds).join(" ");

function RateInput({ label, seconds, onChange }: {
  label: string;
  seconds: number;
  onChange: (seconds: number) => void;
}) {
  const [unit, setUnit] = useState(seconds % 60 === 0 ? "minutes" : "seconds");
  const scale = unit === "minutes" ? 60 : 1;
  return (
    <div className="field">
      <span>{label}</span>
      <div className="interval-input">
        <input
          required
          aria-label={label}
          type="number"
          min={1 / scale}
          max={86400 / scale}
          step={unit === "minutes" ? "any" : 1}
          value={seconds / scale || ""}
          onChange={(event) => onChange(Math.round(Number(event.target.value) * scale))}
        />
        <select aria-label={`${label} unit`} value={unit}
          onChange={(event) => setUnit(event.target.value)}>
          <option value="seconds">seconds</option>
          <option value="minutes">minutes</option>
        </select>
      </div>
    </div>
  );
}

function suggestedWindow(windows: TimeWindow[], interval_seconds: number): TimeWindow {
  const occupied = Array.from({ length: 1440 }, (_, minute) => windows.some(window =>
    window.enabled && (window.start_minute < window.end_minute
      ? minute >= window.start_minute && minute < window.end_minute
      : minute >= window.start_minute || minute < window.end_minute)));
  const after = windows.at(-1)?.end_minute ?? 360;
  for (let offset = 0; offset < 1440; offset++) {
    const start = (after + offset) % 1440;
    if (occupied[start]) continue;
    let length = 1;
    while (length < 60 && !occupied[(start + length) % 1440]) length++;
    return { enabled: true, start_minute: start, end_minute: (start + length) % 1440, interval_seconds };
  }
  return { enabled: false, start_minute: 360, end_minute: 420, interval_seconds };
}

const CatalogContext=createContext<Catalog|null>(null);
function PurchaseLinks({ product, partNumber, carrier="unlocked" }: { product: string; partNumber?:string; carrier?:string }) {
  const catalog=useContext(CatalogContext);
  const variants=catalog?.devices.flatMap(d=>d.variants)??[];
  const variant=variants.find(v=>v.part_number===partNumber && v.carrier===carrier)??variants.find(v=>v.part_number===partNumber);
  const links = variant ? {standard:variant.standard_url,education:variant.education_url} : purchaseLinks(product);
  const [error, setError] = useState("");
  if (!links) return null;
  return (
    <div className="purchase-links">
      {[
        ["Apple Store", links.standard],
        ["Education Store", links.education],
      ].map(([label, url]) => (
        <a
          key={label}
          href={url}
          target="_blank"
          rel="noopener noreferrer"
          aria-label={`${label}: ${product}`}
          onClick={async (event) => {
            if (!native) return;
            event.preventDefault();
            setError("");
            try {
              await openUrl(url);
            } catch {
              setError(
                "Could not open your browser. Copy the link to open it.",
              );
            }
          }}
        >
          {label} <ArrowUpRight size={13} />
        </a>
      ))}
      {error && <small role="alert">{error}</small>}
    </div>
  );
}

export default function App() {
  const [catalog,setCatalog]=useState<Catalog|null>(null);
  const [catalogLoading,setCatalogLoading]=useState(false);
  const [catalogError,setCatalogError]=useState("");
  const [newPageUrl,setNewPageUrl]=useState("");
  const setupPrompted=useRef(false);
  const [historyKey,setHistoryKey]=useState("");
  const [historyView,setHistoryView]=useState<HistoryView|null>(null);
  const [historyTargets,setHistoryTargets]=useState<HistoryTarget[]>([]);
  const [page, setPage] = useState<Page>("overview");
  const [status, setStatus] = useState<Status | null>(null);
  const [latest, setLatest] = useState<Snapshot | null>(null);
  const [lastAvailable, setLastAvailable] = useState<LastAvailable[]>([]);
  const [points, setPoints] = useState<Point[]>([]);
  const [patterns, setPatterns] = useState<PatternInsights | null>(null);
  const [patternsLoading, setPatternsLoading] = useState(true);
  const [patternsError, setPatternsError] = useState("");
  const [patternRefresh, setPatternRefresh] = useState(0);
  const [range, setRange] = useState<Range>("week");
  const [from, setFrom] = useState(localInput(Date.now() - 7 * DAY));
  const [to, setTo] = useState(localInput(Date.now()));
  const [storeId, setStoreId] = useState("");
  const [product, setProduct] = useState("");
  const [now, setNow] = useState(Date.now());
  const [busy, setBusy] = useState(false);
  const [openingAppleSession, setOpeningAppleSession] = useState(false);
  const [reconnectingAppleSession, setReconnectingAppleSession] = useState(false);
  const [appleSessionMessage, setAppleSessionMessage] = useState("");
  const [error, setError] = useState("");
  const [dismissedAttempt, setDismissedAttempt] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [draft, setDraft] = useState<Settings | null>(null);
  const [saving, setSaving] = useState(false);
  const [selected, setSelected] = useState<Snapshot | null>(null);
  const [logPage, setLogPage] = useState(0);
  const [availableOnly, setAvailableOnly] = useState(false);
  const [purchaseAvailableOnly, setPurchaseAvailableOnly] = useState(true);
  const [exported, setExported] = useState("");
  const [notificationMessage, setNotificationMessage] = useState("");
  const [testingNotification, setTestingNotification] = useState(false);
  const generation = useRef(0);

  const startMs =
    range === "custom"
      ? new Date(from).getTime()
      : now - (range === "day" ? DAY : range === "month" ? 30 * DAY : 7 * DAY);
  const endMs = range === "custom" ? new Date(to).getTime() + 59_999 : now;
  const validRange =
    Number.isFinite(startMs) && Number.isFinite(endMs) && endMs >= startMs;

  const refresh = useCallback(async () => {
    const token = ++generation.current;
    try {
      const [nextStatus, nextView] = await Promise.all([
        call<Status>("get_status"),
        call<HistoryView>("get_history_view",{historyKey:historyKey||null,startMs:validRange?startMs:0,endMs:validRange?endMs:0,storeId,product}),
      ]);
      const nextLatest=nextView.latest;
      if (token !== generation.current) return;
      setStatus(nextStatus);
      if(native && !nextStatus.settings.target.zip_code && !setupPrompted.current) {
        setupPrompted.current=true;
        setDraft({...nextStatus.settings,enabled:true,timezone:Intl.DateTimeFormat().resolvedOptions().timeZone});
        setSettingsOpen(true);
      }
      setHistoryView(nextView);
      setHistoryTargets(nextView.targets);
      setLatest(nextLatest);
      setPoints(validRange?nextView.points:[]);
      setLastAvailable(nextView.last_available);
      if (!nextStatus.last_error) {
        setError((previous) =>
          previous.startsWith("The source returned an inventory error:") ||
          previous.startsWith("Inventory request failed:") ||
          previous.startsWith("Apple inventory") ||
            previous.startsWith("Apple storefront") ||
            previous.startsWith("Apple browser") ||
            previous.startsWith("Apple checks") ||
            previous.startsWith("Apple supplied Retry-After")
            ? ""
            : previous,
        );
      }
    } catch (e) {
      if (token === generation.current) setError(errorText(e));
    } finally {
      if (token === generation.current) setLoading(false);
    }
  }, [startMs, endMs, storeId, product, validRange, historyKey]);

  useEffect(()=>{void call<Catalog>("get_catalog").then(setCatalog).catch(e=>setCatalogError(errorText(e)));},[]);
  async function refreshCatalog(category:string) {
    setCatalogLoading(true);setCatalogError("");
    try {setCatalog(await call<Catalog>("refresh_catalog",{category}));}catch(e){setCatalogError(errorText(e));}
    finally {setCatalogLoading(false);}
  }
  async function addCatalogPage() {
    setCatalogLoading(true);setCatalogError("");
    try {setCatalog(await call<Catalog>("add_catalog_page",{url:newPageUrl}));setNewPageUrl("");}catch(e){setCatalogError(errorText(e));}
    finally {setCatalogLoading(false);}
  }
  useEffect(() => {
    void refresh();
  }, [refresh]);
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  useEffect(() => {
    const interval = window.setInterval(() => setNow(Date.now()), 30_000);
    if (!native) return () => clearInterval(interval);
    const subscriptions = [
      listen("inventory-updated", () => {
        setNow(Date.now());
        void refreshRef.current();
      }),
      listen("inventory-checking", () => {
        void refreshRef.current();
      }),
      listen<string>("inventory-error", (event) => setError(event.payload)),
      listen<string>("notification-error", (event) => setError(event.payload)),
    ];
    return () => {
      clearInterval(interval);
      for (const subscription of subscriptions)
        void subscription.then((unlisten) => unlisten());
    };
  }, []);
  useEffect(() => {
    setLogPage(0);
  }, [range, from, to, storeId, product]);

  const patternSource = status?.settings.source;
  const patternUrl = historyKey || status?.settings.url;
  useEffect(() => {
    if (!patternSource || !patternUrl) return;
    let cancelled = false;
    let generation = 0;
    setPatterns(null);
    setPatternsLoading(true);
    async function loadPatterns() {
      const token = ++generation;
      const end = range === "custom" ? new Date(to).getTime() + 59_999 : Date.now();
      const start = range === "custom" ? new Date(from).getTime()
        : end - (range === "day" ? DAY : range === "month" ? 30 * DAY : 7 * DAY);
      if (!Number.isFinite(start) || !Number.isFinite(end) || start > end) {
        setPatternsLoading(false);
        return;
      }
      try {
        const result = await call<PatternInsights>("get_patterns", { startMs: start, endMs: end, storeId, product, historyKey:patternUrl });
        if (!cancelled && token === generation) { setPatterns(result); setPatternsError(""); }
      } catch (error) {
        if (!cancelled && token === generation) setPatternsError(errorText(error));
      } finally {
        if (!cancelled && token === generation) setPatternsLoading(false);
      }
    }
    void loadPatterns();
    // Patterns need new evidence, not a recalculation on every sub-minute collector event.
    const timer = window.setInterval(() => void loadPatterns(), 60_000);
    return () => { cancelled = true; clearInterval(timer); };
  }, [range, from, to, storeId, product, patternSource, patternUrl, patternRefresh]);

  const rows = useMemo(
    () =>
      (latest?.inventory ?? []).filter(
        (r) =>
          (!storeId || r.store_id === storeId) &&
          (!product || r.product === product),
      ),
    [latest, storeId, product],
  );
  const stores = useMemo(
    () =>
      Array.from(
        new Map(
          (latest?.inventory ?? []).map((r) => [r.store_id, r.store]),
        ).entries(),
      ),
    [latest],
  );
  const products = useMemo(
    () =>
      Array.from(new Set(latest?.inventory.map((r) => r.product) ?? [])).sort(),
    [latest],
  );
  const availabilityUnknown = !!historyView?.last_error && (historyView.selected.last_attempt ?? 0) >= (latest?.observed_at ?? 0);
  const available = rows.filter((r) => r.status === "available").length;
  const purchaseConfigurations = Array.from(
    new Set([
      ...rows.map((row) => row.product),
      ...lastAvailable.map((sighting) => sighting.inventory.product),
    ]),
  )
    .map((name) => ({
      name,
      locations: rows.filter(
        (row) => !availabilityUnknown && row.product === name && row.status === "available",
      ),
      previous: lastAvailable.filter(
        (sighting) => sighting.inventory.product === name,
      ),
    }))
    .filter((configuration) =>
      purchaseAvailableOnly
        ? configuration.locations.length > 0 ||
          configuration.previous.length > 0
        : true,
    )
    .sort(
      (a, b) =>
        Number(b.locations.length > 0) - Number(a.locations.length > 0) ||
        (b.previous[0]?.observed_at ?? 0) - (a.previous[0]?.observed_at ?? 0) ||
        a.name.localeCompare(b.name),
    );
  const successes = points.filter((p) => !p.error && p.total > 0);
  const uniqueSources = new Set(
    successes.map((p) => p.source_at ?? p.observed_at),
  ).size;
  const storeRows = Array.from(new Set(rows.map((r) => r.store_id))).map(
    (id) => {
      const entries = rows.filter((r) => r.store_id === id);
      return {
        ...entries[0],
        total: entries.length,
        available: entries.filter((r) => r.status === "available").length,
      };
    },
  );
  const intervalSeconds = status?.settings.interval_seconds ?? 900;
  const chartData = useMemo(() => {
    const data: {
      at: number;
      available: number | null;
      error: string | null;
    }[] = [];
    points.forEach((point, index) => {
      const previous = points[index - 1];
      const expectedAt =
        previous?.next_check_at ??
        (previous?.observed_at ?? 0) + intervalSeconds * 1000;
      const expectedInterval = previous
        ? Math.max(1000, expectedAt - previous.observed_at)
        : intervalSeconds * 1000;
      if (
        index &&
        point.observed_at - previous.observed_at > expectedInterval * 1.8
      ) {
        data.push({
          at: expectedAt,
          available: null,
          error: "No check recorded",
        });
      }
      data.push({
        at: point.observed_at,
        available: point.error || !point.total ? null : point.available,
        error: point.error,
      });
    });
    return data;
  }, [points, intervalSeconds]);
  const latestAttempt = points.at(-1);
  const stale =
    latest && now - (latest.source_at ?? latest.observed_at) > 30 * 60_000;
  const selectedStore = stores.find(([id]) => id === storeId)?.[1];
  const sourceError =
    status?.last_attempt !== dismissedAttempt ? status?.last_error : null;
  const visibleError = error || sourceError || "";

  async function saveExport(name: string, content: string, type?: string) {
    try {
      const path=await exportFile(name, content, type);
      setExported(path);
      return path;
    } catch (e) {
      setError(errorText(e));
      return null;
    }
  }

  async function checkNow() {
    setBusy(true);
    setError("");
    try {
      const snapshot = await call<Snapshot>("check_now");
      if (snapshot.error) setError(snapshot.error);
      setNow(Date.now());
      await refresh();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function reconnectAppleSession() {
    setReconnectingAppleSession(true);
    setBusy(true);
    setError("");
    try {
      const snapshot=await call<Snapshot>("reconnect_apple_session");
      if(snapshot.error)setError(snapshot.error);
      else setAppleSessionMessage("Apple check succeeded; fresh inventory was saved.");
      setNow(Date.now());
      await refresh();
    } catch(error){setError(errorText(error));}
    finally {setReconnectingAppleSession(false);setBusy(false);}
  }
  async function manageAppleSession(hide = false) {
    setOpeningAppleSession(true);
    try {
      await call<void>(hide ? "hide_apple_session" : "open_apple_session");
      setAppleSessionMessage(hide ? "" : "Apple session opened. Review pickup availability there, then use Check now in Orchard.");
      await refresh();
    } catch (error) { setError(errorText(error)); }
    finally { setOpeningAppleSession(false); await refresh(); }
  }

  async function saveSettings(settings: Settings) {
    setSaving(true);
    setError("");
    try {
      if (settings.notify_available) await ensureNotificationPermission();
      const next = await call<Status>("save_settings", { settings });
      if (status?.settings.url !== next.settings.url) {
        setStoreId("");
        setProduct("");
        setHistoryKey("");
      }
      setStatus(next);
      setSettingsOpen(false);
      setNow(Date.now());
      await refresh();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setSaving(false);
    }
  }

  async function ensureNotificationPermission() {
    if (!native)
      throw new Error("Open the desktop app to enable notifications.");
    if (
      !(await isPermissionGranted()) &&
      (await requestPermission()) !== "granted"
    ) {
      throw new Error(
        "Enable Orchard in macOS System Settings → Notifications to receive availability alerts.",
      );
    }
  }

  async function testNotification() {
    setTestingNotification(true);
    setNotificationMessage("");
    try {
      await ensureNotificationPermission();
      await call<void>("test_notification");
      setNotificationMessage(
        "Test sent. If no banner appears, enable Orchard in System Settings → Notifications.",
      );
    } catch (error) {
      setNotificationMessage(errorText(error));
    } finally {
      setTestingNotification(false);
    }
  }

  function exportHistory() {
    const header = [
      "Checked at",
      "Source updated at",
      "Source",
      "Device",
      "Configuration",
      "ZIP code",
      "Carrier",
      "Tracker identity",
      "Available configurations",
      "Total configurations",
      "Error",
    ];
    const data = points.map((p) => [
      new Date(p.observed_at).toISOString(),
      p.source_at ? new Date(p.source_at).toISOString() : "",
      p.source,
      historyView?.selected.device_name??"",
      historyView?.selected.configuration??"",
      historyView?.selected.target?.zip_code??"",
      historyView?.selected.target?.carrier??"",
      historyView?.selected.key??"",
      p.error ? "" : p.available,
      p.error ? "" : p.total,
      p.error ?? "",
    ]);
    void saveExport(
      `orchard-${historyView?.selected.target?.device_id??"history"}-${new Date().toISOString().slice(0, 10)}.csv`,
      [header, ...data].map((r) => r.map(csv).join(",")).join("\n"),
    );
  }

  const historyTarget=historyView?.selected;
  const viewingActive=historyTarget?.key===status?.settings.url;
  const collectorName=catalog?.devices.find(d=>d.id===status?.settings.target.device_id)?.name??"selected device";
  const historyColor=historyTarget?.category==="ipad"?"#587ab0":historyTarget?.category==="mac"?"#9363a7":"#548a68";
  function selectHistory(key:string) {
    generation.current++;
    setHistoryKey(key);setStoreId("");setProduct("");setLogPage(0);setSelected(null);
    setLatest(null);setPoints([]);setLastAvailable([]);setPatterns(null);setHistoryView(null);setLoading(true);
  }
  const isChecking = busy || status?.busy;
  const appleRequestWaiting = status?.settings.source === "apple" && Math.max(status.apple_request_after ?? 0,status.server_retry_until ?? 0)>now;
  const appleCoolingDown = appleRequestWaiting && status?.apple_request_limit_reason === "cooldown";
  const reversedPoints = [...points].reverse();
  const logPoints = reversedPoints.slice(logPage * 25, (logPage + 1) * 25);

  return (
    <CatalogContext.Provider value={catalog}><div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-icon">
            <Smartphone size={22} />
          </div>
          <div>
            orchard<span>INVENTORY, OBSERVED.</span>
          </div>
        </div>
        <div className="workspace-label">YOUR WORKSPACE</div>
        <nav>
          <button
            className={page === "overview" ? "nav-item active" : "nav-item"}
            onClick={() => setPage("overview")}
          >
            <LayoutDashboard size={18} />
            Overview
          </button>
          <button
            className={page === "log" ? "nav-item active" : "nav-item"}
            onClick={() => setPage("log")}
          >
            <Activity size={18} />
            Check log<span className="nav-count">{points.length}</span>
          </button>
          <button
            className="nav-item"
            onClick={() => {
              setDraft(status?.settings ?? null);
              setSettingsOpen(true);
            }}
          >
            <Settings2 size={18} />
            Settings
          </button>
        </nav>
        <div className="sidebar-bottom">
          <div className="local-badge">
            <Database size={15} /> Stored on this Mac
          </div>
          <p>
            Your inventory history stays local.
            <br />
            Every check adds a new observation.
          </p>
          <div className="version">
            Orchard <span>v0.2.1</span>
          </div>
        </div>
      </aside>
      <main className="main">
        <header className="topbar">
          <div className="breadcrumb">
            Workspace <span>/</span>{" "}
            <strong>{page === "overview" ? "Overview" : "Check log"}</strong>
          </div>
          <div className="topbar-right">
            <span
              className={`status-dot ${status?.settings.enabled ? "" : "paused"}`}
            />
            {isChecking
              ? "Checking inventory…"
              : status?.settings.enabled
                ? appleCoolingDown ? "Collector cooling down" : "Collector running"
                : native
                  ? "Collector paused"
                  : "Browser preview"}
          </div>
        </header>
        <div className="content">
          {!native && (
            <div className="preview-banner">
              Browser preview · showing a real captured snapshot. Open the
              desktop app to collect and save history.
            </div>
          )}
          <div className="page-heading">
            <div>
              <div className="eyebrow">A LITTLE CLARITY, ON REPEAT</div>
              <h1>
                {page === "overview"
                  ? "Availability overview"
                  : "Every check, recorded."}
              </h1>
              <p>
                {page === "overview"
                  ? "Watch Apple pickup availability. Find your moment."
                  : "A timestamped record of inventory checks, including failures."}
              </p>
            </div>
            <button
              className="primary-button"
              onClick={() => void checkNow()}
              disabled={!!isChecking || !native || appleRequestWaiting}
            >
              {isChecking ? (
                <LoaderCircle size={16} className="spin" />
              ) : (
                <RefreshCw size={16} />
              )}
              Check now
            </button>
          </div>
          {visibleError && (
            <div className="error-banner" role="alert">
              <div className="error-content">
                {historyTarget && !viewingActive && sourceError ? `Collector (${collectorName}): ` : ""}{describeError(visibleError)}
                {(sourceError ||
                  visibleError.startsWith(
                    "The source returned an inventory error:",
                  ) ||
                  visibleError.startsWith("Inventory request failed:") ||
                  visibleError.startsWith("Apple inventory")) && (
                  <small style={{ display: "block", marginTop: 4 }}>
                    {status?.settings.enabled
                      ? `Next scheduled check: ${status.next_due ? dateTime(status.next_due) : "shortly"}. ${status.server_retry_until > now ? "Apple requested this delay." : appleRequestWaiting ? "Manual retries wait for the same pacing or cooldown period." : "You can also use Check now."}`
                      : "Collection is paused. Resume it or use Check now to retry."}
                  </small>
                )}
                {native && status?.settings.source === "apple" && (
                  <div className="apple-session-actions">
                    <button className="secondary-button" disabled={openingAppleSession || appleRequestWaiting} onClick={() => void manageAppleSession()}>
                      {openingAppleSession ? "Opening…" : "Open Apple session"}
                    </button>
                    <button className="secondary-button" disabled={!!isChecking || reconnectingAppleSession || appleRequestWaiting} onClick={()=>void reconnectAppleSession()}>{reconnectingAppleSession?"Retrying…":"Retry & check"}</button>
                    <button className="text-button" onClick={()=>{setDraft(status.settings);setSettingsOpen(true);}}>Connection details</button>
                    {appleCoolingDown && <small>Orchard is giving Apple a quiet period after a blocked request. Manual and scheduled checks resume together.</small>}
                  </div>
                )}
              </div>
              <button
                aria-label="Dismiss error"
                onClick={() => {
                  setError("");
                  setDismissedAttempt(status?.last_attempt ?? null);
                }}
              >
                <X size={16} />
              </button>
            </div>
          )}
          {appleSessionMessage && <div className="preview-banner" role="status">{appleSessionMessage}</div>}
          {native && status?.apple_session_open && (
            <div className="preview-banner">
              Apple session is open for you to inspect.
              <button className="text-button" disabled={openingAppleSession} onClick={() => void manageAppleSession(true)}>Return to background</button>
            </div>
          )}
          {exported && (
            <div className="preview-banner" role="status">
              Export saved: {exported}{" "}
              <button
                className="icon-button"
                aria-label="Dismiss export message"
                onClick={() => setExported("")}
              >
                <X size={13} />
              </button>
            </div>
          )}
          <section className="target-bar history-target-bar" style={{borderLeft:`4px solid ${historyColor}`}}>
            <div className="target-icon"><Smartphone size={23}/></div>
            <div className="history-picker">
              <label htmlFor="history-device">History device</label>
              <select id="history-device" aria-label="History device" value={historyKey} onChange={e=>selectHistory(e.target.value)}>
                <option value="">{collectorName} · active tracker</option>
                {historyTargets.filter(t=>!t.active || t.key===historyKey).map(t=><option key={t.key} value={t.key}>{t.device_name} · {t.configuration} · {t.target?.carrier??"imported"} · ZIP {t.target?.zip_code??"—"}</option>)}
              </select>
              <span>{historyTarget ? `${historyTarget.configuration} · ${historyTarget.target?.carrier??"imported"} · ZIP ${historyTarget.target?.zip_code||"not set"}` : "Loading saved history…"}</span>
            </div>
            <button onClick={()=>{setDraft(status?.settings??null);setSettingsOpen(true);}}>Edit active tracker <ArrowUpRight size={14}/></button>
          </section>
          {historyTarget && <div className="history-context" role="status">
            <span style={{color:historyColor}}>{historyTarget.checks} saved checks · {historyTarget.last_attempt?`last observed ${dateTime(historyTarget.last_attempt)}`:"no observations yet"}</span>
            {!viewingActive && <span>Viewing saved {historyTarget.device_name} history. The collector {status?.settings.enabled?"watches":"is paused on"} {collectorName}; Check now checks {collectorName}.</span>}
            {historyView?.last_error && !viewingActive && <span className="amber-text">Last check for this device failed: {historyView.last_error}</span>}
          </div>}
          <div className="filter-bar">
            <div className="range-tabs" aria-label="Date range">
              {(
                [
                  ["day", "24 hours"],
                  ["week", "7 days"],
                  ["month", "30 days"],
                  ["custom", "Custom"],
                ] as [Range, string][]
              ).map(([value, label]) => (
                <button
                  key={value}
                  className={range === value ? "selected" : ""}
                  onClick={() => setRange(value)}
                >
                  {label}
                </button>
              ))}
            </div>
            <div className="filters">
              <label className="select-wrap">
                <MapPin size={14} />
                <select
                  aria-label="Store"
                  value={storeId}
                  onChange={(e) => setStoreId(e.target.value)}
                >
                  <option value="">All stores</option>
                  {stores.map(([id, name]) => (
                    <option value={id} key={id}>
                      {name}
                    </option>
                  ))}
                </select>
                <ChevronDown size={13} />
              </label>
              <label className="select-wrap product-select">
                <select
                  aria-label="Configuration"
                  value={product}
                  onChange={(e) => setProduct(e.target.value)}
                >
                  <option value="">All configurations</option>
                  {products.map((name) => (
                    <option key={name}>{name}</option>
                  ))}
                </select>
                <ChevronDown size={13} />
              </label>
            </div>
          </div>
          {range === "custom" && (
            <div className="custom-range">
              <CalendarDays size={16} />
              <label>
                From
                <input
                  aria-label="Start date"
                  type="datetime-local"
                  value={from}
                  onInput={(e) => setFrom(e.currentTarget.value)}
                />
              </label>
              <span>→</span>
              <label>
                To
                <input
                  aria-label="End date"
                  type="datetime-local"
                  value={to}
                  onInput={(e) => setTo(e.currentTarget.value)}
                />
              </label>
              {!validRange && (
                <span className="invalid-range">
                  Choose an end after the start.
                </span>
              )}
            </div>
          )}
          {page === "overview" ? (
            <>
              <div className="stats-grid">
                <article className="stat-card">
                  <div className="stat-label">
                    Saved availability
                    <Smartphone size={16} />
                  </div>
                  <div className="stat-value">
                    {availabilityUnknown ? "Unknown" : latest ? available : "—"}
                    <span>{availabilityUnknown ? ` · last known ${available} / ${rows.length}` : ` / ${rows.length} configurations`}</span>
                  </div>
                  <div className={`stat-foot ${available && !availabilityUnknown ? "green-text" : ""}`}>
                    <span className={`tiny-dot ${available && !availabilityUnknown ? "green" : ""}`} />
                    {availabilityUnknown ? "Latest check failed; current availability is unknown" : latest
                      ? available
                        ? "Pickup available in latest snapshot"
                        : "No pickup availability in latest snapshot"
                      : "Waiting for the first check"}
                  </div>
                  {available > 0 && !availabilityUnknown && (
                    <button
                      className="text-button available-shortcut"
                      onClick={() => {
                        setAvailableOnly(true);
                        setSelected(latest);
                      }}
                    >
                      View available <ArrowUpRight size={13} />
                    </button>
                  )}
                </article>
                <article className="stat-card">
                  <div className="stat-label">
                    Checks in range
                    <Activity size={16} />
                  </div>
                  <div className="stat-value">
                    {points.length}
                    <span> observations</span>
                  </div>
                  <div className="stat-foot">
                    {uniqueSources} distinct snapshots ·{" "}
                    {points.filter((p) => p.error).length} failed
                  </div>
                </article>
                <article className="stat-card">
                  <div className="stat-label">
                    Stores in view
                    <MapPin size={16} />
                  </div>
                  <div className="stat-value">
                    {storeRows.length}
                    <span> locations</span>
                  </div>
                  <div className="stat-foot">
                    {selectedStore ?? "Near your selected ZIP code"}
                  </div>
                </article>
              </div>
              <section className="panel chart-panel">
                <div className="panel-header">
                  <div>
                    <h2>{historyTarget?.device_name??"Device"} availability over time</h2>
                    <p>{historyTarget?.configuration??"Selected configurations"} · configurations available for pickup at each check</p>
                  </div>
                  <div className="chart-legend">
                    <span style={{background:historyColor}} /> {historyTarget?.device_name??"Available"}{" "}
                    <button
                      className="icon-button"
                      title="Export filtered history"
                      aria-label="Export filtered history"
                      onClick={exportHistory}
                      disabled={!points.length}
                    >
                      <ArrowDownToLine size={17} />
                    </button>
                  </div>
                </div>
                {loading ? (
                  <div className="chart-empty">
                    <LoaderCircle className="spin" size={22} />
                    <h3>Loading your observations</h3>
                  </div>
                ) : !successes.length || !validRange ? (
                  <div className="chart-empty">
                    <div className="empty-icon">
                      <Activity size={26} />
                    </div>
                    <h3>
                      {points.length
                        ? "No inventory data in this view"
                        : "Your history starts here"}
                    </h3>
                    <p>
                      {points.length
                        ? "Try another filter, or inspect the check log for errors."
                        : "Each scheduled check adds a point. Earlier dates stay empty until collected."}
                    </p>
                  </div>
                ) : (
                  <div className="chart">
                    <ResponsiveContainer width="100%" height={260}>
                      <ComposedChart
                        data={chartData}
                        margin={{ top: 20, right: 16, left: -24, bottom: 4 }}
                      >
                        <defs>
                          <linearGradient
                            id="availableFill"
                            x1="0"
                            y1="0"
                            x2="0"
                            y2="1"
                          >
                            <stop
                              offset="0%"
                              stopColor={historyColor}
                              stopOpacity={0.22}
                            />
                            <stop
                              offset="100%"
                              stopColor={historyColor}
                              stopOpacity={0.015}
                            />
                          </linearGradient>
                        </defs>
                        <CartesianGrid
                          vertical={false}
                          stroke="#e9ece6"
                          strokeDasharray="3 4"
                        />
                        <XAxis
                          dataKey="at"
                          type="number"
                          domain={[startMs, endMs]}
                          tickFormatter={(ms) =>
                            range === "day"
                              ? time(ms)
                              : new Date(ms).toLocaleDateString([], {
                                  month: "short",
                                  day: "numeric",
                                })
                          }
                          axisLine={false}
                          tickLine={false}
                          tick={{ fill: "#879187", fontSize: 11 }}
                          minTickGap={50}
                        />
                        <YAxis
                          allowDecimals={false}
                          domain={[
                            0,
                            Math.max(
                              4,
                              ...successes.map((p) => p.available + 1),
                            ),
                          ]}
                          axisLine={false}
                          tickLine={false}
                          tick={{ fill: "#879187", fontSize: 11 }}
                        />
                        <Tooltip
                          labelFormatter={(ms) => dateTime(Number(ms))}
                          formatter={(value) => [value, "Available"]}
                          contentStyle={{
                            border: "1px solid #e5e9e1",
                            borderRadius: 12,
                            fontSize: 12,
                          }}
                          cursor={{ stroke: "#8caa96", strokeDasharray: "4 4" }}
                        />
                        <Area
                          type="stepAfter"
                          dataKey="available"
                          stroke={historyColor}
                          strokeWidth={2}
                          fill="url(#availableFill)"
                          connectNulls={false}
                          dot={{
                            r: 3,
                            fill: historyColor,
                            stroke: "#fff",
                            strokeWidth: 1,
                          }}
                          activeDot={{ r: 5 }}
                        />
                      </ComposedChart>
                    </ResponsiveContainer>
                  </div>
                )}
                <div className="chart-footer">
                  <span>
                    <span className="tiny-dot green" />
                    {successes.length === 1
                      ? "First observation recorded. The graph will grow with each check."
                      : `${successes.length} successful observations in this range`}
                  </span>
                  <span>Gaps & failures stay unfilled</span>
                </div>
              </section>
              <section className="panel purchase-panel">
                <div className="panel-header">
                  <div>
                    <h2>
                      {purchaseAvailableOnly
                        ? "Available for pickup"
                        : "Buy unlocked"}
                    </h2>
                    <p>
                      {latest
                        ? `${availabilityUnknown ? "Last successful check" : "Latest check"} ${dateTime(latest.observed_at)} · matching unlocked purchase links`
                        : "Your first check will show available configurations here."}
                    </p>
                  </div>
                  <label className="purchase-filter">
                    <input
                      type="checkbox"
                      checked={!purchaseAvailableOnly}
                      onChange={(event) =>
                        setPurchaseAvailableOnly(!event.target.checked)
                      }
                    />
                    Show all configurations
                  </label>
                </div>
                <div className="purchase-list">
                  {available === 0 && lastAvailable.length > 0 && (
                    <p className="purchase-history-note">
                      {availabilityUnknown ? "Current availability is unknown. Greyed-out items were available previously." : "None available in the latest check. Greyed-out items were available previously."}
                    </p>
                  )}
                  {purchaseConfigurations.map(
                    ({ name, locations, previous }) => (
                      <div
                        className={`purchase-row ${!locations.length && previous.length ? "past-availability" : ""}`}
                        key={name}
                      >
                        <div className="purchase-configuration">
                          <strong>{name}</strong>
                          {locations.length ? (
                            locations.map((row) => (
                              <span
                                className="purchase-store"
                                key={row.store_id}
                              >
                                <MapPin size={12} />
                                {row.store} · {row.distance}
                                {row.pickup_quote && ` · ${row.pickup_quote}`}
                              </span>
                            ))
                          ) : previous.length ? (
                            <>
                              <span className="purchase-last-seen">
                                Last seen available{" "}
                                {dateTime(previous[0].observed_at)}
                              </span>
                              {previous.map(({ inventory: row }) => (
                                <span
                                  className="purchase-store"
                                  key={row.store_id}
                                >
                                  <MapPin size={12} />
                                  {row.store} · {row.distance}
                                </span>
                              ))}
                            </>
                          ) : (
                            <span className="purchase-unavailable">
                              {availabilityUnknown ? "Current pickup availability is unknown" : "No pickup availability in the latest check"}
                            </span>
                          )}
                        </div>
                        <PurchaseLinks product={name} carrier={historyTarget?.target?.carrier} partNumber={locations[0]?.part_number ?? previous[0]?.inventory.part_number ?? rows.find(row=>row.product===name)?.part_number} />
                      </div>
                    ),
                  )}
                  {!purchaseConfigurations.length && (
                    <div className="purchase-empty">
                      <p>
                        {loading
                          ? "Loading the latest inventory…"
                          : !latest
                            ? "Waiting for the first successful check."
                            : purchaseAvailableOnly && rows.length
                              ? "No pickup availability has been recorded for these configurations yet."
                              : "No configurations match your store and configuration filters."}
                      </p>
                      {purchaseAvailableOnly && rows.length > 0 && (
                        <button
                          className="text-button"
                          onClick={() => setPurchaseAvailableOnly(false)}
                        >
                          Show all configurations <ArrowUpRight size={13} />
                        </button>
                      )}
                    </div>
                  )}
                </div>
              </section>
              <PatternPanel timezone={status?.settings.timezone ?? "America/Los_Angeles"} insights={patterns} loading={patternsLoading} error={patternsError}
                onRefresh={() => setPatternRefresh(value => value + 1)}
                onConfigure={() => {
                  if (status) { setDraft(status.settings); setSettingsOpen(true); }
                }} />
              <div className="bottom-grid">
                <section className="panel stores-panel">
                  <div className="panel-header">
                    <div>
                      <h2>{availabilityUnknown ? "Last successful inventory by store" : "Latest inventory by store"}</h2>
                      <p>
                        {latest
                          ? `Checked ${dateTime(latest.observed_at)}`
                          : "Your first check will populate this list"}
                      </p>
                    </div>
                    <span className="subtle-badge">
                      {storeRows.length} stores
                    </span>
                  </div>
                  <div className="store-table">
                    <div className="store-table-head">
                      <span>STORE</span>
                      <span>AVAILABLE / CHECKED</span>
                    </div>
                    {storeRows.slice(0, 6).map((store) => (
                      <div className="store-row" key={store.store_id}>
                        <div className="store-name">
                          <div className="store-pin">
                            <MapPin size={15} />
                          </div>
                          <div>
                            <strong>{store.store}</strong>
                            <span>
                              {store.city} · {store.distance}
                            </span>
                          </div>
                        </div>
                        <span
                          className={`availability-pill ${store.available ? "in-stock" : ""}`}
                        >
                          {store.available} / {store.total}
                          <span className="tiny-dot" />
                        </span>
                      </div>
                    ))}
                    {!storeRows.length && (
                      <div className="small-empty">No stores recorded yet.</div>
                    )}
                  </div>
                  {storeRows.length > 6 && (
                    <button
                      className="panel-link"
                      onClick={() => setSelected(latest)}
                    >
                      View all {storeRows.length} stores{" "}
                      <ArrowUpRight size={14} />
                    </button>
                  )}
                </section>
                <section className="panel schedule-panel">
                  <div className="panel-header">
                    <div>
                      <h2>Last check · {collectorName}</h2>
                    </div>
                    <Clock3 size={19} />
                  </div>
                  <div className="schedule-interval">
                    Every {rateParts(status?.active_interval_seconds ?? intervalSeconds)[0]}
                    <span>{rateParts(status?.active_interval_seconds ?? intervalSeconds)[1]}</span>
                  </div>
                  {status?.settings.source === "apple" && status.configured_interval_seconds < status.active_interval_seconds && <p className="connection-note">Requested {rateParts(status.configured_interval_seconds).join(" ")}; Apple checks are paced to at least one minute.</p>}
                  <div className="schedule-row">
                    <span>Next check</span>
                    <strong>
                      {status?.next_due
                        ? status.next_due <= now
                          ? "Due now"
                          : time(status.next_due)
                        : "Paused"}
                    </strong>
                  </div>
                  <div className="schedule-row">
                    <span>Last check</span>
                    <strong>
                      {status?.last_attempt
                        ? time(status.last_attempt)
                        : "Not yet"}
                    </strong>
                  </div>
                  {viewingActive && (
                  <div className="schedule-row">
                    <span>
                      {latest?.source === "apple"
                        ? "Apple response checked"
                        : "Source updated"}
                    </span>
                    <strong className={stale ? "amber-text" : ""}>
                      {latest?.source === "apple"
                        ? time(latest.observed_at)
                        : latest?.source_at
                          ? time(latest.source_at)
                          : "Unknown"}
                      {stale ? " · stale" : ""}
                    </strong>
                  </div>
                  )}
                  {status?.settings.windows.filter(window => window.enabled).map((window, index) => (
                    <p className="window-status" key={index}>
                      <Clock3 size={13} />
                      {clockInput(window.start_minute)}–{clockInput(window.end_minute)} {status?.settings.timezone}
                      {window.end_minute < window.start_minute ? " (overnight)" : ""} · every {rateLabel(window.interval_seconds)}
                    </p>
                  ))}
                  <div className="schedule-actions">
                    <button
                      className="secondary-button"
                      onClick={() =>
                        status &&
                        void saveSettings({
                          ...status.settings,
                          enabled: !status.settings.enabled,
                        })
                      }
                      disabled={!native || saving}
                    >
                      {status?.settings.enabled ? (
                        <Pause size={14} />
                      ) : (
                        <Play size={14} />
                      )}{" "}
                      {status?.settings.enabled ? "Pause" : "Resume"}
                    </button>
                    <button
                      className="text-button"
                      onClick={() => {
                        setDraft(status?.settings ?? null);
                        setSettingsOpen(true);
                      }}
                    >
                      Configure <ArrowUpRight size={13} />
                    </button>
                  </div>
                  <p className="schedule-note">
                    Close the window to keep collecting in the menu bar. Checks
                    pause while your Mac sleeps or the app is quit.
                  </p>
                </section>
              </div>
            </>
          ) : (
            <section className="panel log-panel">
              <div className="panel-header">
                <div>
                  <h2>{historyTarget?.device_name??"Device"} check history</h2>
                  <p>
                    {points.length} checks · {dateTime(startMs)} to{" "}
                    {dateTime(endMs)}
                  </p>
                </div>
                <button
                  className="secondary-button"
                  onClick={exportHistory}
                  disabled={!points.length}
                >
                  <ArrowDownToLine size={15} />
                  Export CSV
                </button>
              </div>
              <div className="log-scroll">
                <table>
                  <thead>
                    <tr>
                      <th>CHECKED AT</th>
                      <th>SOURCE UPDATED</th>
                      <th>DATA SOURCE</th>
                      <th>RESULT</th>
                      <th>AVAILABLE</th>
                      <th />
                    </tr>
                  </thead>
                  <tbody>
                    {logPoints.map((point) => (
                      <tr key={point.id}>
                        <td>{dateTime(point.observed_at)}</td>
                        <td>
                          {point.source_at ? dateTime(point.source_at) : "—"}
                        </td>
                        <td>
                          {point.source === "apple"
                            ? "Apple"
                            : "Inventory Checker"}
                        </td>
                        <td>
                          <span
                            className={
                              point.error ? "result-error" : "result-ok"
                            }
                          >
                            {point.error ? "Failed" : "Recorded"}
                          </span>
                          {point.error && (
                            <span className="log-error" title={point.error}>
                              {point.error}
                            </span>
                          )}
                        </td>
                        <td>
                          {point.error
                            ? "—"
                            : `${point.available} / ${point.total}`}
                        </td>
                        <td>
                          <button
                            className="text-button"
                            aria-label={`Details for check ${point.id}`}
                            onClick={() => {
                              setAvailableOnly(false);
                              void call<Snapshot>("get_snapshot", {
                                id: point.id,
                              })
                                .then(setSelected)
                                .catch((e) => setError(errorText(e)));
                            }}
                          >
                            Details <ArrowUpRight size={13} />
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
                {!points.length && (
                  <div className="small-empty">
                    No checks recorded in this date range.
                  </div>
                )}
              </div>
              <div className="pagination">
                <span>
                  {points.length
                    ? `${logPage * 25 + 1}–${Math.min((logPage + 1) * 25, points.length)} of ${points.length}`
                    : "0 checks"}
                </span>
                <div>
                  <button
                    className="secondary-button"
                    disabled={!logPage}
                    onClick={() => setLogPage((p) => p - 1)}
                  >
                    Previous
                  </button>
                  <button
                    className="secondary-button"
                    disabled={(logPage + 1) * 25 >= points.length}
                    onClick={() => setLogPage((p) => p + 1)}
                  >
                    Next
                  </button>
                </div>
              </div>
            </section>
          )}
          <footer className="content-footer">
            <span>
              <Database size={13} /> Local history · no account needed
            </span>
            <span>
              {latestAttempt?.error
                ? "Last check in range failed · see log"
                : status?.settings.source === "apple"
                  ? "Source: Apple"
                  : "Source: Legacy imported history"}
              <ArrowUpRight size={12} />
            </span>
          </footer>
        </div>
      </main>
      {settingsOpen && draft && (
        <div
          className="modal-backdrop"
          onClick={(e) => {
            if (e.target === e.currentTarget && !saving) setSettingsOpen(false);
          }}
        >
          <section
            className="modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="settings-title"
          >
            <div className="modal-header">
              <div>
                <div className="eyebrow">MAKE IT YOURS</div>
                <h2 id="settings-title">Tracker settings</h2>
              </div>
              <button
                className="icon-button"
                aria-label="Close settings"
                onClick={() => setSettingsOpen(false)}
                disabled={saving}
              >
                <X size={20} />
              </button>
            </div>
            <form
              onSubmit={(e) => {
                e.preventDefault();
                void saveSettings(draft);
              }}
            >
              <DeviceSelector catalog={catalog} settings={draft} onChange={setDraft} onRefresh={refreshCatalog} refreshing={catalogLoading} error={catalogError}/>
              <details className="catalog-extra"><summary>Add a newer Apple buy page</summary>
                <p className="connection-note">Use a US iPhone, iPad, or Mac family page. Orchard reads its configurations and keeps them in the local catalog.</p>
                <input aria-label="Apple buy page" type="url" placeholder="https://www.apple.com/shop/buy-mac/macbook-air" value={newPageUrl} onChange={e=>setNewPageUrl(e.target.value)}/>
                <button type="button" className="text-button" disabled={!native||catalogLoading||!newPageUrl} onClick={()=>void addCatalogPage()}>Read Apple page</button>
              </details>
              <label className="field">Connection<select aria-label="Apple connection method" value={draft.connection} onChange={e=>setDraft({...draft,connection:e.target.value as Settings["connection"]})}>
                <option value="auto">Automatic Apple endpoints</option><option value="direct">Direct pickup endpoint</option><option value="browser">Apple storefront browser session</option>
              </select><small>Methods share the same pacing and cooldown. Browser sessions require Chrome or Edge; they do not add request quota.</small></label>
              <div className="notification-test"><button type="button" className="text-button" disabled={!native||openingAppleSession||appleRequestWaiting} onClick={()=>void manageAppleSession()}>Open Apple session</button></div>
              {native && <ConnectionPanel onReconnect={()=>void reconnectAppleSession()} connecting={reconnectingAppleSession} disabled={!!isChecking||reconnectingAppleSession||openingAppleSession||appleRequestWaiting} onExport={saveExport}/>}
              <label className="field">Schedule time zone<input required aria-label="Schedule time zone" list="time-zones" value={draft.timezone} onChange={e=>setDraft({...draft,timezone:e.target.value})}/>
                <datalist id="time-zones">{["America/Los_Angeles","America/Denver","America/Chicago","America/New_York","America/Phoenix","Pacific/Honolulu","UTC"].map(zone=><option key={zone} value={zone}/>)}</datalist>
                <small>Daily time windows follow this IANA time zone, including daylight-saving changes.</small>
              </label>
              <RateInput label="Default check rate" seconds={draft.interval_seconds}
                onChange={(interval_seconds) => setDraft({ ...draft, interval_seconds })} />
              <p className="schedule-help">
                Used outside enabled time windows. Choose 1 second to 24 hours.

              </p>
              <label className="toggle-field">
                <div>
                  <strong>Scheduled collection</strong>
                  <small>
                    Checks run while Orchard is open and your computer is awake.
                  </small>
                </div>
                <input
                  type="checkbox"
                  checked={draft.enabled}
                  onChange={(e) =>
                    setDraft({ ...draft, enabled: e.target.checked })
                  }
                />
              </label>
              <label className="toggle-field">
                <div>
                  <strong>Availability notifications</strong>
                  <small>
                    Notify when a model becomes available at a store. Unchanged
                    availability stays quiet.
                  </small>
                </div>
                <input
                  aria-label="Availability notifications"
                  type="checkbox"
                  checked={draft.notify_available}
                  onChange={(event) =>
                    setDraft({
                      ...draft,
                      notify_available: event.target.checked,
                    })
                  }
                />
              </label>
              <div className="notification-test">
                <button
                  type="button"
                  className="text-button"
                  disabled={!native || testingNotification}
                  onClick={() => void testNotification()}
                >
                  {testingNotification ? "Sending…" : "Send test notification"}
                </button>
                {notificationMessage && (
                  <p role="status">{notificationMessage}</p>
                )}
              </div>
              <div className="windows-config">
                <div className="windows-heading">
                  <div>
                    <strong>Time windows</strong>
                    <p className="schedule-help">Repeat daily in the selected time zone, with daylight-saving adjustments. Overnight windows are supported; enabled windows cannot overlap.</p>
                  </div>
                  <button type="button" className="secondary-button"
                    onClick={() => setDraft({ ...draft, windows: [...draft.windows, suggestedWindow(draft.windows, draft.interval_seconds)] })}>
                    Add window
                  </button>
                </div>
                {!draft.windows.length && <p className="schedule-help">No windows yet. The default rate applies all day.</p>}
                {draft.windows.map((window, index) => {
                  const update = (patch: Partial<TimeWindow>) => setDraft({
                    ...draft,
                    windows: draft.windows.map((current, i) => i === index ? { ...current, ...patch } : current),
                  });
                  return (
                    <fieldset className={`time-window${window.enabled ? "" : " disabled-window"}`} key={index}>
                      <legend>Window {index + 1}</legend>
                      <div className="window-controls">
                        <label><input type="checkbox" aria-label={`Enable window ${index + 1}`}
                          checked={window.enabled} onChange={event => update({ enabled: event.target.checked })} /> Enabled</label>
                        <button type="button" className="text-button" aria-label={`Remove window ${index + 1}`}
                          onClick={() => setDraft({ ...draft, windows: draft.windows.filter((_, i) => i !== index) })}>Remove</button>
                      </div>
                      <div className="window-fields">
                        <label className="field">From
                          <input required aria-label={`Window ${index + 1} start time`} type="time"
                            value={clockInput(window.start_minute)}
                            onInput={event => update({ start_minute: clockMinutes(event.currentTarget.value) })} />
                        </label>
                        <label className="field">Until
                          <input required aria-label={`Window ${index + 1} end time`} type="time"
                            value={clockInput(window.end_minute)}
                            onInput={event => update({ end_minute: clockMinutes(event.currentTarget.value) })} />
                        </label>
                        <RateInput key={`${index}-${draft.windows.length}`} label={`Window ${index + 1} rate`}
                          seconds={window.interval_seconds} onChange={interval_seconds => update({ interval_seconds })} />
                      </div>
                      {window.end_minute < window.start_minute && <p className="schedule-help">Ends the following day.</p>}
                    </fieldset>
                  );
                })}
              </div>
              <div className="storage-info">
                <Database size={17} />
                <div>
                  <strong>History saved locally</strong>
                  <code>{status?.db_path}</code>
                  <span>
                    History is kept until you remove the database. Each device,
                    configuration, and ZIP code keeps its own history.
                  </span>
                </div>
              </div>
              {error && (
                <p className="form-error" role="alert">
                  {error}
                </p>
              )}
              <div className="modal-actions">
                <button
                  type="button"
                  className="secondary-button"
                  onClick={() => setSettingsOpen(false)}
                >
                  Cancel
                </button>
                <button
                  type="submit"
                  className="primary-button"
                  disabled={saving || !native}
                >
                  {saving ? (
                    <LoaderCircle size={15} className="spin" />
                  ) : (
                    <Check size={15} />
                  )}
                  Save settings
                </button>
              </div>
            </form>
          </section>
        </div>
      )}
      {selected && (
        <div
          className="modal-backdrop"
          onClick={(e) => {
            if (e.target === e.currentTarget) setSelected(null);
          }}
        >
          <section
            className="modal detail-modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="details-title"
          >
            <div className="modal-header">
              <div>
                <div className="eyebrow">OBSERVATION #{selected.id}</div>
                <h2 id="details-title">{dateTime(selected.observed_at)}</h2>
                <p>
                  {selected.source === "apple"
                    ? "Apple (direct) · collected "
                    : "Legacy import · source updated "}
                  {selected.source_at
                    ? dateTime(selected.source_at)
                    : dateTime(selected.observed_at)}
                </p>
              </div>
              <button
                className="icon-button"
                aria-label="Close details"
                onClick={() => setSelected(null)}
              >
                <X size={20} />
              </button>
            </div>
            {selected.error ? (
              <div className="error-banner">{selected.error}</div>
            ) : (
              <>
                <div className="detail-toolbar">
                  <label>
                    <input
                      type="checkbox"
                      checked={availableOnly}
                      onChange={(e) => setAvailableOnly(e.target.checked)}
                    />{" "}
                    Available only
                  </label>
                  <button
                    className="secondary-button"
                    onClick={() =>
                      void saveExport(
                        `orchard-snapshot-${selected.id}.json`,
                        JSON.stringify(selected, null, 2),
                        "application/json",
                      )
                    }
                  >
                    <ArrowDownToLine size={14} />
                    Export JSON
                  </button>
                </div>
                <div className="detail-list">
                  {selected.inventory
                    .filter(
                      (r) =>
                        (!storeId || r.store_id === storeId) &&
                        (!product || r.product === product) &&
                        (!availableOnly || r.status === "available"),
                    )
                    .map((row: Inventory) => (
                      <div
                        className="detail-row"
                        key={`${row.store_id}-${row.part_number}`}
                      >
                        <div>
                          <strong>{row.product}</strong>
                          <span>
                            {row.store} · {row.part_number}
                          </span>
                          <PurchaseLinks product={row.product} partNumber={row.part_number} carrier={selected.url.split("/").at(-1)?.split("?")[0]} />
                        </div>
                        <div
                          className={
                            row.status === "available" ? "green-text" : ""
                          }
                        >
                          <strong>{row.status}</strong>
                          <span>{row.pickup_quote}</span>
                        </div>
                      </div>
                    ))}
                  {availableOnly &&
                    !selected.inventory.some(
                      (r) =>
                        (!storeId || r.store_id === storeId) &&
                        (!product || r.product === product) &&
                        r.status === "available",
                    ) && (
                      <div className="small-empty">
                        No available configurations in this snapshot.
                      </div>
                    )}
                </div>
              </>
            )}
          </section>
        </div>
      )}
    </div></CatalogContext.Provider>
  );
}
