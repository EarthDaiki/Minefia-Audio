import { useState } from "react";
import { CircleDollarSign, RefreshCw, X } from "lucide-react";
import type { ProjectCosts } from "../types";

export function UsageModal({ open, costs, loading, error, onRefresh, onClose }: { open: boolean; costs: ProjectCosts | null; loading: boolean; error: string; onRefresh: () => void; onClose: () => void }) {
  const [sortOrder, setSortOrder] = useState<"date-asc" | "date-desc" | "amount-asc" | "amount-desc">("date-desc");
  if (!open) return null;
  const days = costs ? [...costs.days].sort((left, right) => {
    switch (sortOrder) {
      case "date-asc": return left.date.localeCompare(right.date);
      case "date-desc": return right.date.localeCompare(left.date);
      case "amount-asc": return left.totalUsd - right.totalUsd;
      case "amount-desc": return right.totalUsd - left.totalUsd;
    }
  }) : [];
  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
    <section className="modal usage-modal" role="dialog" aria-modal="true" aria-labelledby="usage-title">
      <header className="usage-header">
        <button className="icon-button modal-close" onClick={onClose} aria-label="Close usage"><X size={18} /></button>
        <div className="modal-icon"><CircleDollarSign size={22} /></div><p className="eyebrow">OPENAI PROJECT COSTS</p><h2 id="usage-title">Project usage</h2>
        <p className="modal-copy">Daily costs for the Project linked to Minefia Audio, grouped by model.</p>
      </header>
      <div className="usage-content">
        <button className="summary-start-button" onClick={onRefresh} disabled={loading}>{loading ? <><RefreshCw className="spin" size={14} /> Loading Costs API…</> : <><RefreshCw size={14} /> Refresh costs</>}</button>
        {error && <div className="error-toast usage-error"><CircleDollarSign size={16} /><span>{error}</span></div>}
        {costs && <><div className="usage-total"><span>Total · last 180 days</span><strong>{costs.currency.toUpperCase()} {costs.totalUsd.toFixed(5)}</strong></div>
          <label className="usage-sort"><span>Sort daily costs</span><select value={sortOrder} onChange={(event) => setSortOrder(event.target.value as typeof sortOrder)} aria-label="Sort daily costs">
            <option value="date-asc">Date · ascending (oldest first)</option>
            <option value="date-desc">Date · descending (newest first)</option>
            <option value="amount-asc">Amount · ascending (lowest first)</option>
            <option value="amount-desc">Amount · descending (highest first)</option>
          </select></label>
          <div className="usage-days">{days.map((day) => <section key={day.date} className="usage-day"><header><h3>{day.date}</h3><strong>{costs.currency.toUpperCase()} {day.totalUsd.toFixed(5)}</strong></header>
          {day.models.map((model) => <div className="usage-model" key={model.model}><strong>{model.model}</strong><b>{costs.currency.toUpperCase()} {model.amountUsd.toFixed(5)}</b></div>)}
          </section>)}{costs.days.length === 0 && <div className="library-empty"><CircleDollarSign size={22} /><strong>No project costs found</strong><span>OpenAI returned no usage in this date range.</span></div>}</div></>}
        <aside className="usage-permission-note"><strong>About access</strong><span>Usage is enabled only after Minefia Audio has a Service Account key for its linked Project. Costs are read from the organization Costs API with line-item grouping, then grouped by model in the app.</span></aside>
      </div>
    </section>
  </div>;
}
