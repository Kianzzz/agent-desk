import type { Settings, Tool } from "../types";

export const TOOL_LABEL: Record<Tool, string> = {
  claude: "Claude",
  codex: "Codex",
  gemini: "Gemini",
};

export const TOOL_ORDER: Tool[] = ["claude", "codex", "gemini"];

/** 每个工具固定一个颜色，筛选后也不变。 */
export function toolVar(tool: string): string {
  return tool === "claude" || tool === "codex" || tool === "gemini" ? `var(--tool-${tool})` : "var(--text-muted)";
}

/** 旧接口保留：费用一律按美元显示。 */
export interface MoneyFmt {
  currency: "USD";
}

export function moneyFmt(_s?: Settings | null): MoneyFmt {
  return { currency: "USD" };
}

/** $0.23、$12.4、$96.6、$1,234 */
export function money(usd: number, _f?: MoneyFmt): string {
  const a = Math.abs(usd);
  if (a === 0) return "$0";
  if (a < 0.01) return "<$0.01";
  if (a < 10) return "$" + usd.toFixed(2);
  if (a < 1000) return "$" + usd.toFixed(1);
  return "$" + Math.round(usd).toLocaleString("en-US");
}

/** 259.4M、672K、1.23B */
export function tokens(n: number): string {
  if (n >= 1e9) return (n / 1e9).toFixed(2) + "B";
  if (n >= 1e6) return (n / 1e6).toFixed(1) + "M";
  if (n >= 1e4) return Math.round(n / 1e3) + "K";
  if (n >= 1e3) return (n / 1e3).toFixed(1) + "K";
  return String(Math.round(n));
}

export function bytes(n: number): string {
  if (n >= 1e12) return (n / 1e12).toFixed(2) + " TB";
  if (n >= 1e9) return (n / 1e9).toFixed(n >= 1e11 ? 0 : 1) + " GB";
  if (n >= 1e6) return (n / 1e6).toFixed(n >= 1e8 ? 0 : 1) + " MB";
  if (n >= 1e3) return (n / 1e3).toFixed(0) + " KB";
  return n + " B";
}

export function int(n: number): string {
  return Math.round(n).toLocaleString("en-US");
}

export function pct(n: number): string {
  return (n >= 10 || n === 0 ? n.toFixed(0) : n.toFixed(1)) + "%";
}

export function localDate(d = new Date()): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

export function addDays(date: string, delta: number): string {
  const [y, m, d] = date.split("-").map(Number);
  return localDate(new Date(y, m - 1, d + delta));
}

const hm = (t: Date) => `${String(t.getHours()).padStart(2, "0")}:${String(t.getMinutes()).padStart(2, "0")}`;
const WEEKDAY = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];

/** 「刚刚」「3 分钟前」「昨天 14:20」「9月12日」 */
export function ago(iso: string | null | undefined): string {
  if (!iso) return "—";
  const t = new Date(iso);
  const diff = (Date.now() - t.getTime()) / 1000;
  if (diff < 60) return "刚刚";
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`;
  if (diff < 86400 && localDate(t) === localDate()) return `${Math.floor(diff / 3600)} 小时前`;
  if (localDate(t) === addDays(localDate(), -1)) return `昨天 ${hm(t)}`;
  if (diff < 86400 * 300) return `${t.getMonth() + 1}月${t.getDate()}日`;
  return `${t.getFullYear()}年${t.getMonth() + 1}月`;
}

/** 额度刷新时间：「2:11 后」「明天 09:00」「周四 10:00」「10月12日」。 */
export function resetText(iso: string | null | undefined): string {
  if (!iso) return "";
  const t = new Date(iso);
  const s = (t.getTime() - Date.now()) / 1000;
  if (s <= 0) return "已重置";
  if (s < 6 * 3600) return `${Math.floor(s / 3600)}:${String(Math.floor((s % 3600) / 60)).padStart(2, "0")} 后`;
  const today = localDate();
  const day = localDate(t);
  if (day === today) return `今天 ${hm(t)}`;
  if (day === addDays(today, 1)) return `明天 ${hm(t)}`;
  if (s < 7 * 86400) return `${WEEKDAY[t.getDay()]} ${hm(t)}`;
  return `${t.getMonth() + 1}月${t.getDate()}日`;
}

export function daysSince(iso: string): number {
  return Math.floor((Date.now() - new Date(iso).getTime()) / 86400000);
}

export function shortDate(date: string): string {
  const [, m, d] = date.split("-").map(Number);
  return `${m}/${d}`;
}

/** 把 /Users/xxx 换成 ~ */
export function tildify(p: string | null | undefined): string {
  if (!p) return "";
  return p.replace(/^\/Users\/[^/]+/, "~");
}

export function basename(p: string | null | undefined): string {
  if (!p) return "";
  const parts = p.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || p;
}

/** 旧接口：Claude 5 小时窗口的剩余时间 */
export function durationLeft(iso: string): string {
  return resetText(iso);
}
