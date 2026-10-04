import { ago, pct, resetText } from "../lib/format";
import type { QuotaWin } from "../types";

/** 分段额度条。用到 70% 变黄、90% 变红；数据过期时变灰。 */
export function SegBar({ value, segments = 10, stale }: { value: number; segments?: number; stale?: boolean }) {
  const v = Math.min(100, Math.max(0, value));
  const on = v > 0 ? Math.max(1, Math.round((v / 100) * segments)) : 0;
  const cls = stale ? " stale" : v >= 90 ? " alert" : v >= 70 ? " warn" : "";
  return (
    <div className={"seg" + cls} role="meter" aria-valuenow={v} aria-valuemin={0} aria-valuemax={100}>
      {Array.from({ length: segments }, (_, i) => (
        <i key={i} className={i < on ? "on" : ""} />
      ))}
    </div>
  );
}

export function QuotaRow({ w, segments }: { w: QuotaWin; segments?: number }) {
  return (
    <div className="q-bar">
      <SegBar value={w.expired ? 0 : w.usedPercent} segments={segments} stale={w.expired} />
      <div className="seg-meta">
        <span>
          {w.label} <b>{w.expired ? "已重置" : pct(w.usedPercent)}</b>
        </span>
        <span className="r" title={w.resetsAt ? new Date(w.resetsAt).toLocaleString() : undefined}>
          {w.expired ? `记录于 ${ago(w.observedAt)}` : resetText(w.resetsAt)}
        </span>
      </div>
    </div>
  );
}
