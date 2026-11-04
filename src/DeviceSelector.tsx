import { useState } from "react";
import { RefreshCw } from "lucide-react";
import { trackerKey, type Catalog, type Settings, type Target } from "./api";

export function DeviceSelector({catalog,settings,onChange,onRefresh,refreshing,error}: {
  catalog:Catalog|null;settings:Settings;onChange:(settings:Settings)=>void;
  onRefresh:(category:string)=>void;refreshing:boolean;error:string;
}) {
  const [search,setSearch]=useState("");
  const device=catalog?.devices.find(d=>d.id===settings.target.device_id);
  const category=device?.category??"iphone";
  const choices=catalog?.devices.filter(d=>d.category===category)??[];
  const variants=device?.variants.filter(v=>v.carrier===settings.target.carrier)??[];
  const selected=variants.find(v=>v.part_number===settings.target.part_number);
  const matches=variants.filter(v=>`${v.title} ${v.part_number}`.toLowerCase().includes(search.toLowerCase())||v.part_number===settings.target.part_number);
  const update=(changes:Partial<Target>)=>{
    const target={...settings.target,...changes};
    onChange({...settings,target,url:trackerKey(target)});
  };
  return <section className="device-selector" aria-label="Device selection">
    <div className="connection-heading"><div><strong>What to watch</strong><p>Apple US pickup inventory for a device and configuration.</p></div>
      <button type="button" className="text-button" disabled={refreshing||!catalog} onClick={()=>onRefresh(category)}><RefreshCw size={14}/>{refreshing?"Refreshing…":"Refresh from Apple"}</button>
    </div>
    <div className="device-fields">
      <label className="field">Category<select aria-label="Device category" value={category} onChange={e=>{const first=catalog?.devices.find(d=>d.category===e.target.value);if(first){setSearch("");update({device_id:first.id,part_number:"",carrier:"unlocked"});}}}>
        <option value="iphone">iPhone</option><option value="ipad">iPad</option><option value="mac">Mac</option>
      </select></label>
      <label className="field">Device<select aria-label="Device" value={settings.target.device_id} disabled={!catalog} onChange={e=>{setSearch("");update({device_id:e.target.value,part_number:"",carrier:"unlocked"});}}>
        {choices.map(d=><option key={d.id} value={d.id}>{d.name}</option>)}
      </select></label>
    </div>
    {category==="iphone" && <label className="field">Carrier<select aria-label="Carrier" value={settings.target.carrier} onChange={e=>update({carrier:e.target.value,part_number:""})}>
      <option value="unlocked">Unlocked</option><option value="att">AT&amp;T</option><option value="tmobile">T-Mobile</option><option value="verizon">Verizon</option>
    </select></label>}
    <label className="field">Find a configuration<input aria-label="Find a configuration" type="search" placeholder="Search capacity, color, size, or chip" value={search} onChange={e=>setSearch(e.target.value)}/></label>
    <label className="field">Configuration<select aria-label="Configuration to watch" value={settings.target.part_number} disabled={!device} onChange={e=>update({part_number:e.target.value})}>
      <option value="">All {variants.length} configurations</option>
      {matches.map(v=><option key={v.part_number} value={v.part_number}>{v.title}{v.details_error?` · ${v.part_number}`:""}{v.coming_soon?" · coming soon":""}</option>)}
    </select></label>
    {selected && <p className="connection-note">{selected.title} · {selected.part_number}{selected.details_error ? ` · ${selected.details_error}` : ""}</p>}
    {category==="mac" && <p className="connection-note">The catalog lists standard sellable Mac configurations. Custom builds without a pickup SKU are excluded.</p>}
    <label className="field">ZIP code<input required aria-label="ZIP code" inputMode="numeric" pattern="[0-9]{5}" maxLength={5} placeholder="Your US ZIP code" value={settings.target.zip_code} onChange={e=>update({zip_code:e.target.value})}/></label>
    {device && <p className="connection-note">Catalog read {device.checked_at_ms?new Date(device.checked_at_ms).toLocaleDateString():"from the bundled snapshot"}.{device.last_error?` Last refresh failed: ${device.last_error}`:""}</p>}
    {error && <p role="alert" className="form-error">{error}</p>}
  </section>;
}
