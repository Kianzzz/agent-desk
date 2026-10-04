import { useLayoutEffect, useRef, useState } from "react";
import { money, shortDate, TOOL_LABEL, tokens as fmtTokens, toolVar, type MoneyFmt } from "../lib/format";
import type { DayPoint, Series } from "../lib/usage";

/** 0 到 max 之间取整齐的刻度。 */
function niceTicks(max: number, count = 4): number[] {
  if (max <= 0) return [0];
  const raw = max / count;
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? raw;
  const ticks: number[] = [];
  for (let v = 0; v <= max + step * 0.001; v += step) ticks.push(v);
  if (ticks[ticks.length - 1] < max) ticks.push(ticks[ticks.length - 1] + step);
  return ticks;
}

function topRounded(x: number, y: number, w: number, h: number, r: number) {
  const rr = Math.min(r, w / 2, h);
  return `M${x},${y + h}V${y + rr}Q${x},${y} ${x + rr},${y}H${x + w - rr}Q${x + w},${y} ${x + w},${y + rr}V${y + h}Z`;
}

export function StackedBars({
  points,
  tools,
  fmt,
  height = 200,
  metric = "tokens",
  onPick,
}: {
  points: DayPoint[];
  tools: Series[];
  fmt?: MoneyFmt;
  height?: number;
  /** 柱子高度按 token 还是按费用 */
  metric?: "tokens" | "cost";
  onPick?: (date: string) => void;
}) {
  const wrap = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(600);
  const [hover, setHover] = useState<number | null>(null);

  useLayoutEffect(() => {
    const el = wrap.current;
    if (!el) return;
    const measure = () => el.clientWidth > 0 && setWidth(el.clientWidth);
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    window.addEventListener("resize", measure);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, []);

  const val = (p: DayPoint, t?: Series) =>
    metric === "tokens" ? (t ? (p.tokensByTool[t] ?? 0) : p.tokens) : t ? (p.byTool[t] ?? 0) : p.total;
  const tick = (v: number) => (metric === "tokens" ? fmtTokens(v) : "$" + (v >= 1000 ? `${v / 1000}k` : v));
  const left = 52;
  const right = 4;
  const top = 8;
  const bottom = 22;
  const innerW = Math.max(10, width - left - right);
  const innerH = height - top - bottom;
  const maxV = Math.max(...points.map((p) => val(p)), 0);
  const ticks = niceTicks(maxV);
  const yMax = ticks[ticks.length - 1] || 1;
  const band = innerW / Math.max(points.length, 1);
  const barW = Math.min(24, Math.max(3, band * 0.62));
  const y = (v: number) => top + innerH - (v / yMax) * innerH;
  const labelEvery = Math.ceil(points.length / Math.max(1, Math.floor(innerW / 46)));

  const hp = hover !== null ? points[hover] : null;
  const tipLeft = hover !== null ? Math.min(left + band * hover + band / 2 + 12, width - 180) : 0;

  return (
    <div className="chart" ref={wrap} onMouseLeave={() => setHover(null)}>
      <svg width={width} height={height} role="img" aria-label="每日费用柱状图">
        <g className="axis">
          {ticks.map((t) => (
            <g key={t}>
              <line className={t === 0 ? "baseline" : "gridline"} x1={left} x2={width - right} y1={y(t)} y2={y(t)} />
              <text x={left - 8} y={y(t) + 3.5} textAnchor="end">
                {tick(t)}
              </text>
            </g>
          ))}
          {points.map((p, i) =>
            (points.length - 1 - i) % labelEvery === 0 ? (
              <text key={p.date} x={left + band * i + band / 2} y={height - 6} textAnchor="middle">
                {shortDate(p.date)}
              </text>
            ) : null,
          )}
        </g>
        {points.map((p, i) => {
          const x = left + band * i + (band - barW) / 2;
          const segs = tools.map((t) => ({ t, v: val(p, t) })).filter((s) => s.v > 0);
          let cursor = y(0);
          return (
            <g key={p.date} opacity={hover === null || hover === i ? 1 : 0.45}>
              {segs.map((s, j) => {
                const h = (s.v / yMax) * innerH;
                const isTop = j === segs.length - 1;
                const drawH = isTop ? h : Math.max(0, h - 2);
                const yTop = cursor - h;
                cursor -= h;
                if (drawH < 0.5) return null;
                return isTop ? (
                  <path key={s.t} d={topRounded(x, yTop, barW, drawH, 4)} fill={toolVar(s.t)} />
                ) : (
                  <rect key={s.t} x={x} y={yTop + (h - drawH)} width={barW} height={drawH} fill={toolVar(s.t)} />
                );
              })}
            </g>
          );
        })}
        {points.map((p, i) => (
          <rect
            key={"hit" + p.date}
            x={left + band * i}
            y={top}
            width={band}
            height={innerH}
            fill="transparent"
            onMouseEnter={() => setHover(i)}
            onClick={() => onPick?.(p.date)}
            style={{ cursor: onPick ? "pointer" : "default" }}
          />
        ))}
      </svg>
      {hp && (
        <div className="tooltip" style={{ left: tipLeft, top: 4 }}>
          <div className="t-head">{hp.date}</div>
          {tools.map((t) =>
            hp.tokensByTool[t] ? (
              <div className="t-row" key={t}>
                <span className="dot" style={{ background: toolVar(t) }} />
                {TOOL_LABEL[t]}
                <b>{fmtTokens(hp.tokensByTool[t]!)}</b>
                <small>{money(hp.byTool[t] ?? 0, fmt)}</small>
              </div>
            ) : null,
          )}
          <div className="t-row" style={{ marginTop: 4, borderTop: "1px solid var(--border)", paddingTop: 4 }}>
            合计
            <b>{fmtTokens(hp.tokens)}</b>
            <small>{money(hp.total, fmt)}</small>
          </div>
        </div>
      )}
    </div>
  );
}

export function ToolLegend({ tools }: { tools: Series[] }) {
  return (
    <div className="legend">
      {tools.map((t) => (
        <span key={t}>
          <i style={{ background: toolVar(t) }} />
          {TOOL_LABEL[t]}
        </span>
      ))}
    </div>
  );
}

/** 迷你柱状（菜单栏卡片里的最近 7 天）。单一系列，不需要图例。 */
export function MiniBars({ values, height = 34, labels }: { values: number[]; height?: number; labels?: string[] }) {
  const max = Math.max(...values, 0) || 1;
  const n = values.length;
  const w = 220;
  const band = w / n;
  const bw = Math.min(16, band * 0.6);
  return (
    <svg width="100%" viewBox={`0 0 ${w} ${height}`} preserveAspectRatio="none" style={{ display: "block" }} aria-hidden>
      {values.map((v, i) => {
        const h = Math.max(v > 0 ? 2 : 0, (v / max) * (height - 2));
        const x = band * i + (band - bw) / 2;
        return (
          <path key={i} d={topRounded(x, height - h, bw, h, 3)} fill={i === n - 1 ? "var(--seg-on)" : "var(--seg-off)"}>
            {labels && <title>{labels[i]}</title>}
          </path>
        );
      })}
    </svg>
  );
}
