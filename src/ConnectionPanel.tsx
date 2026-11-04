import { useCallback, useEffect, useState } from "react";
import { call } from "./api";
import type { ConnectionReport } from "./api";
import { LoaderCircle, RefreshCw } from "lucide-react";

const date = (at: number) => new Date(at).toLocaleString([], {year:"numeric",month:"short",day:"numeric",hour:"numeric",minute:"2-digit",second:"2-digit"});
const age = (at: number, start: number | null) => {
  if (!start) return "—";
  const minutes = Math.max(0,Math.floor((at-start)/60000));
  return minutes >= 60 ? `${Math.floor(minutes/60)}h ${minutes%60}m` : `${minutes}m`;
};
export function ConnectionPanel({ onReconnect, connecting, disabled, onExport }: {
  onReconnect: () => void;
  connecting: boolean;
  disabled: boolean;
  onExport: (name: string, content: string) => Promise<string | null>;
}) {
  const [report,setReport] = useState<ConnectionReport | null>(null);
  const [loading,setLoading] = useState(false);
  const [error,setError] = useState("");
  const [exported,setExported] = useState("");
  const refresh = useCallback(async () => {
    setLoading(true);
    try { setReport(await call<ConnectionReport>("get_connection_diagnostics")); setError(""); }
    catch(error) { setError(String(error)); }
    finally { setLoading(false); }
  },[]);
  useEffect(() => { void refresh(); const timer=setInterval(()=>void refresh(),5000); return ()=>clearInterval(timer); },[refresh]);
  useEffect(()=>{ if (!connecting) void refresh(); },[connecting,refresh]);
  const latest=report?.latest;
  const sample=latest?.after ?? latest?.before;
  return <section className="connection-panel" aria-label="Apple connection diagnostics">
    <div className="connection-heading">
      <div><strong>Apple connection</strong><p>Inspect the last inventory request or retry the connection.</p></div>
      <button type="button" className="icon-button" aria-label="Refresh connection diagnostics" disabled={loading} onClick={()=>void refresh()}>
        {loading ? <LoaderCircle size={15} className="spin"/> : <RefreshCw size={15}/>}
      </button>
    </div>
    <div className="connection-actions">
      <button type="button" className="secondary-button" disabled={disabled} onClick={onReconnect}>{connecting?"Retrying…":"Retry & check"}</button>
      <button type="button" className="text-button" disabled={!report} onClick={async()=>{
        try { const content=await call<string>("export_connection_diagnostics"); const path=await onExport("orchard-apple-connection.json",content); if(path)setExported(path); }
        catch(error){setError(String(error));}
      }}>Export diagnostic log</button>
    </div>
    {latest ? <>
      <p className="connection-outcome">{latest.success ? "Last request succeeded" : "Last request failed"} · {date(latest.at_ms)}{latest.operation==="reconnect"?" · after retry":latest.operation==="session_renewal"?" · after scheduled session refresh":""}</p>
      <dl className="connection-stats">
        {latest.response.endpoint && <div><dt>Inventory endpoint</dt><dd>{latest.response.endpoint}{latest.response.transport==="http"?" · direct HTTP":" · browser session"}</dd></div>}
        <div><dt>HTTP response</dt><dd>{latest.response.http_status ?? "—"}{latest.response.content_type ? ` · ${latest.response.content_type}`:""}{latest.response.body_kind && latest.response.body_kind!=="json"?` · ${latest.response.body_kind.replaceAll("_"," ")}`:""}</dd></div>
        <div><dt>Last successful response</dt><dd>{report?.last_success_at_ms?date(report.last_success_at_ms):"Not recorded yet"}</dd></div>
        <div><dt>Request + diagnostics</dt><dd>{latest.elapsed_ms!==null?`${(latest.elapsed_ms/1000).toFixed(2)} seconds`:"—"}</dd></div>
        {latest.response.transport!=="http" && <>
          <div><dt>Profile / browser age at check</dt><dd>{age(latest.at_ms,latest.profile_created_at_ms)} / {age(latest.at_ms,latest.browser_started_at_ms)}</dd></div>
          <div><dt>Page age / visibility</dt><dd>{age(latest.at_ms,latest.page_loaded_at_ms)} · {sample?.page.visibility??"unknown"}</dd></div>
          <div><dt>Cookie metadata before / after</dt><dd>{latest.before?.cookies_available?latest.before.cookies.length:"unknown"} / {latest.after?.cookies_available?latest.after.cookies.length:"unknown"}</dd></div>
        </>}
        <div><dt>Saved observation</dt><dd>{latest.snapshot_id?`#${latest.snapshot_id}`:"Not linked yet"}{latest.next_check_at_ms?` · next ${date(latest.next_check_at_ms)}`:""}</dd></div>
        {!!latest.response.retry_until_ms && <div><dt>Apple retry delay</dt><dd>{date(latest.response.retry_until_ms)}</dd></div>}
      </dl>
      {!!latest.expired_since_previous_sample.length && <p className="connection-note">Expired since the previous same-profile sample: {latest.expired_since_previous_sample.join(", ")}. This does not establish which cookie caused a failure.</p>}
      {latest.response.transport!=="http" && <details>
        <summary>Cookie expiry metadata ({sample?.cookies.length ?? 0})</summary>
        <p className="connection-note">Names and expiry times only. Session cookies have no browser expiry timestamp; Apple can still invalidate them server-side.</p>
        <div className="connection-table"><table><thead><tr><th>Cookie</th><th>Expiry</th></tr></thead><tbody>
          {sample?.cookies.map(cookie=><tr key={`${cookie.domain}-${cookie.name}`}><td>{cookie.name}</td><td>{cookie.expires_at_ms?date(cookie.expires_at_ms):"Session cookie"}</td></tr>)}
        </tbody></table></div>
      </details>}
    </> : <p className="connection-note">The next Apple check will record connection and expiry metadata here.</p>}
    <details className="connection-recent"><summary>Recent connection events</summary>
      {report?.recent.slice(-12).reverse().map((event,index)=><p key={index}>{date(event.at_ms)} · {event.event.replaceAll("_"," ")}{event.response.http_status?` · HTTP ${event.response.http_status}`:""}{event.success===false?" · failed":""}</p>)}
    </details>
    <p className="connection-note">Logs stay on this Mac and rotate at about 16 MB. Successful checks are written at most once a minute; failures and reconnects are always written. Cookie values, raw response bodies, and authentication headers are excluded.</p>
    {report && !report.logging_available && <p className="form-error">Connection metadata is available in memory, but the log file could not be written.</p>}
    {error && <p className="form-error" role="alert">{error}</p>}
    {exported && <p className="connection-export" role="status">Saved: {exported}</p>}
  </section>;
}
