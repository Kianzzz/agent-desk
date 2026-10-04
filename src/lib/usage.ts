import type { Account, DailyModelRow, TokenCounts, Tool, UsageSnapshot } from "../types";
import { addDays, localDate, TOOL_ORDER } from "./format";

export const emptyTokens = (): TokenCounts => ({ input: 0, output: 0, cacheRead: 0, cacheWrite: 0, reasoning: 0 });

export function addTokens(a: TokenCounts, b: TokenCounts): TokenCounts {
  return {
    input: a.input + b.input,
    output: a.output + b.output,
    cacheRead: a.cacheRead + b.cacheRead,
    cacheWrite: a.cacheWrite + b.cacheWrite,
    reasoning: a.reasoning + b.reasoning,
  };
}

export const totalTokens = (t: TokenCounts) => t.input + t.output + t.cacheRead + t.cacheWrite;

export interface Totals {
  cost: number;
  tokens: TokenCounts;
  requests: number;
  unpriced: number;
}

const emptyTotals = (): Totals => ({ cost: 0, tokens: emptyTokens(), requests: 0, unpriced: 0 });

function addRow(t: Totals, r: DailyModelRow) {
  t.cost += r.costUsd;
  t.tokens = addTokens(t.tokens, r.tokens);
  t.requests += r.requests;
  t.unpriced += r.unpricedRequests;
}

export function rowsInRange(snap: UsageSnapshot, from: string | null, to: string = localDate()) {
  return snap.days.filter((d) => (from === null || d.date >= from) && d.date <= to);
}

export function sumRows(rows: DailyModelRow[]): Totals {
  const t = emptyTotals();
  rows.forEach((r) => addRow(t, r));
  return t;
}

export function byTool(rows: DailyModelRow[]): Map<Tool, Totals> {
  const m = new Map<Tool, Totals>();
  for (const r of rows) {
    if (!m.has(r.tool)) m.set(r.tool, emptyTotals());
    addRow(m.get(r.tool)!, r);
  }
  return new Map(TOOL_ORDER.filter((t) => m.has(t)).map((t) => [t, m.get(t)!]));
}

export interface ModelTotals extends Totals {
  tool: Tool;
  model: string;
}

export function byModel(rows: DailyModelRow[]): ModelTotals[] {
  const m = new Map<string, ModelTotals>();
  for (const r of rows) {
    const k = r.tool + "|" + r.model;
    if (!m.has(k)) m.set(k, { tool: r.tool, model: r.model, ...emptyTotals() });
    addRow(m.get(k)!, r);
  }
  return [...m.values()].sort((a, b) => b.cost - a.cost || totalTokens(b.tokens) - totalTokens(a.tokens));
}

/** 图表里的一个系列：某个工具，或者合并起来的「其他」。 */
export type Series = Tool | "other";

export interface DayPoint {
  date: string;
  /** 费用合计（美元） */
  total: number;
  byTool: Partial<Record<Series, number>>;
  /** token 合计 */
  tokens: number;
  tokensByTool: Partial<Record<Series, number>>;
}

/** 连续的日期序列（没用量的日子补 0）。 */
export function dailySeries(snap: UsageSnapshot, days: number, tools?: Set<Tool>): DayPoint[] {
  const today = localDate();
  const from = addDays(today, -(days - 1));
  const map = new Map<string, DayPoint>();
  for (let i = 0; i < days; i++) {
    const d = addDays(from, i);
    map.set(d, { date: d, total: 0, byTool: {}, tokens: 0, tokensByTool: {} });
  }
  for (const r of snap.days) {
    const p = map.get(r.date);
    if (!p || (tools && !tools.has(r.tool))) continue;
    p.byTool[r.tool] = (p.byTool[r.tool] ?? 0) + r.costUsd;
    p.total += r.costUsd;
    const t = totalTokens(r.tokens);
    p.tokensByTool[r.tool] = (p.tokensByTool[r.tool] ?? 0) + t;
    p.tokens += t;
  }
  return [...map.values()];
}

export function firstDate(snap: UsageSnapshot): string | null {
  return snap.days.length ? snap.days[0].date : null;
}

export function todayAndYesterday(snap: UsageSnapshot) {
  const today = localDate();
  const yesterday = addDays(today, -1);
  return {
    today: sumRows(snap.days.filter((d) => d.date === today)),
    yesterday: sumRows(snap.days.filter((d) => d.date === yesterday)),
    todayRows: snap.days.filter((d) => d.date === today),
  };
}

export function monthRows(snap: UsageSnapshot) {
  const prefix = localDate().slice(0, 7);
  return snap.days.filter((d) => d.date.startsWith(prefix));
}

/** 工具超过 max 个时，按 token 保留前 max-1 个，其余合并成「其他」，免得颜色不够、图例太长。 */
export function foldSeries(points: DayPoint[], tools: Tool[], max = 6): { points: DayPoint[]; series: Series[] } {
  if (tools.length <= max) return { points, series: tools };
  const sum = (t: Tool) => points.reduce((a, p) => a + (p.tokensByTool[t] ?? 0), 0);
  const keep = new Set([...tools].sort((a, b) => sum(b) - sum(a)).slice(0, max - 1));
  const folded = points.map((p) => {
    const byTool: Partial<Record<Series, number>> = {};
    const tokensByTool: Partial<Record<Series, number>> = {};
    for (const t of tools) {
      const k: Series = keep.has(t) ? t : "other";
      if (p.byTool[t]) byTool[k] = (byTool[k] ?? 0) + p.byTool[t]!;
      if (p.tokensByTool[t]) tokensByTool[k] = (tokensByTool[k] ?? 0) + p.tokensByTool[t]!;
    }
    return { ...p, byTool, tokensByTool };
  });
  return { points: folded, series: [...tools.filter((t) => keep.has(t)), "other"] };
}

/** 有用量、但账号模块没覆盖到的工具，补一条「只有用量」的账号，好让它也有一张卡片。 */
export function withUsageOnly(accounts: Account[] | null, tools: Iterable<Tool>): Account[] {
  const list = [...(accounts ?? [])];
  for (const t of tools) {
    if (!list.some((a) => a.tool === t)) {
      list.push({ tool: t, loggedIn: true, loginKind: null, plan: null, windows: [], quotaSource: null, hint: "这个工具不在本机记录账号等级和额度" });
    }
  }
  return list.sort((a, b) => TOOL_ORDER.indexOf(a.tool) - TOOL_ORDER.indexOf(b.tool));
}
