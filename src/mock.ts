// 浏览器预览用的数据。`src/mock-data/` 里有真实导出的 JSON 就用它（该目录不入库），否则用生成的假数据。
import type { DailyModelRow, DiskReport, Inventory, ScanReport, Settings, UsageSnapshot } from "./types";
import { addDays, localDate } from "./lib/format";

const real = import.meta.glob("./mock-data/*.json");

async function loadReal<T>(name: string): Promise<T | null> {
  const loader = real[`./mock-data/${name}.json`];
  if (!loader) return null;
  const mod = (await loader()) as { default: T };
  return mod.default;
}

let settings: Settings = {
  theme: "dark",
  edgeEnabled: true,
  edgeSide: "right",
  trayEnabled: true,
  trayShowCost: true,
  currency: "USD",
  cnyRate: 7.1,
  subscriptions: [{ tool: "claude", name: "Claude Max 20x", monthlyUsd: 200 }],
  ignoredFindings: [],
  launchAtLogin: false,
  refreshSeconds: 60,
};

function fakeUsage(): UsageSnapshot {
  const today = localDate();
  const days: DailyModelRow[] = [];
  for (let i = 44; i >= 0; i--) {
    const date = addDays(today, -i);
    const k = 0.5 + Math.abs(Math.sin(i * 1.7));
    days.push({
      date,
      tool: "claude",
      model: "claude-opus-5-5",
      tokens: { input: 4000 * k, output: 90000 * k, cacheRead: 9e6 * k, cacheWrite: 6e5 * k, reasoning: 20000 * k },
      costUsd: 18 * k,
      requests: Math.round(300 * k),
      unpricedRequests: 0,
    });
    if (i % 3 !== 1)
      days.push({
        date,
        tool: "codex",
        model: i % 2 ? "gpt-6-sol" : "gpt-6-astra",
        tokens: { input: 80000 * k, output: 30000 * k, cacheRead: 3e6 * k, cacheWrite: 0, reasoning: 9000 * k },
        costUsd: 6 * k,
        requests: Math.round(90 * k),
        unpricedRequests: 0,
      });
  }
  const now = new Date().toISOString();
  return {
    generatedAt: now,
    timezone: "Asia/Shanghai",
    days,
    projects: [
      { project: "/Users/me/Projects/blog", tool: "claude", tokens: days[0].tokens, costUsd: 312.4, requests: 5200, sessions: 21, firstActive: now, lastActive: now },
      { project: "/Users/me/Notes", tool: "codex", tokens: days[0].tokens, costUsd: 120.8, requests: 1800, sessions: 14, firstActive: now, lastActive: now },
    ],
    sessions: [
      { sessionId: "a", tool: "claude", project: "/Users/me/Documents/agent-desk", title: "做一个本地 AI Agent 管理工具", models: ["claude-opus-5-5"], startedAt: now, lastActive: now, tokens: days[0].tokens, costUsd: 24.1, requests: 210 },
    ],
    quotas: [{ tool: "codex", label: "每周额度", usedPercent: 38, windowMinutes: 10080, resetsAt: new Date(Date.now() + 3 * 86400000).toISOString(), observedAt: now }],
    activeBlocks: [
      { tool: "claude", start: new Date(Date.now() - 2 * 3600000).toISOString(), end: new Date(Date.now() + 3 * 3600000).toISOString(), costUsd: 14.2, tokens: days[0].tokens, requests: 120, burnRateUsdPerHour: 7.1 },
    ],
    sources: [{ tool: "claude", root: "/Users/me/.claude/projects", files: 60, records: 19000, archivedRecords: 0, lastRecordAt: now, errors: [] }],
    pricingUpdatedAt: now,
    unpricedModels: [],
  };
}

export async function mockInvoke(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
  await new Promise((r) => setTimeout(r, 120));
  switch (cmd) {
    case "get_usage":
    case "refresh_usage":
      return { snapshot: (await loadReal<UsageSnapshot>("usage")) ?? fakeUsage(), error: null };
    case "get_accounts":
      return [
        { tool: "claude", loggedIn: true, loginKind: "Claude 订阅", plan: "Max 5x", quotaSource: "Claude Code 状态栏", hint: null,
          windows: [
            { kind: "fiveHour", label: "5 小时", usedPercent: 42, resetsAt: new Date(Date.now() + 2.2 * 3600000).toISOString(), observedAt: new Date().toISOString(), expired: false, estimated: false, recordedPercent: 0 },
            { kind: "weekly", label: "本周", usedPercent: 18, resetsAt: new Date(Date.now() + 3 * 86400000).toISOString(), observedAt: new Date().toISOString(), expired: false, estimated: false, recordedPercent: 0 },
          ] },
        { tool: "codex", loggedIn: true, loginKind: "ChatGPT 账号", plan: "Pro", quotaSource: "Codex 会话日志", hint: null,
          windows: [{ kind: "weekly", label: "本周", usedPercent: 1, resetsAt: new Date(Date.now() + 5 * 86400000).toISOString(), observedAt: new Date().toISOString(), expired: false, estimated: false, recordedPercent: 0 }] },
        { tool: "gemini", loggedIn: true, loginKind: "Google 账号", plan: null, quotaSource: null, hint: "Gemini CLI 不在本机记录账号等级和额度", windows: [] },
      ];
    case "claude_bridge_status":
      return "notInstalled";
    case "get_inventory":
      return (await loadReal<Inventory>("inventory")) ?? { scannedAt: new Date().toISOString(), mcp: [], skillRoots: [], skills: [], skillGroups: [], hooks: [], plugins: [], warnings: [] };
    case "get_security":
    case "run_security_scan":
      return (await loadReal<ScanReport>("security")) ?? { scannedAt: new Date().toISOString(), filesScanned: 0, durationMs: 0, findings: [] };
    case "get_disk":
    case "run_disk_scan":
      return (await loadReal<DiskReport>("disk")) ?? null;
    case "get_settings":
      return settings;
    case "save_settings":
      settings = args!.settings as Settings;
      return settings;
    case "update_pricing":
      return 1234;
    case "assess_items":
      return [];
    case "trash_paths":
      return [];
    case "read_skill_md":
      return "---\nname: 示例\n---\n# 预览";
    default:
      return null;
  }
}
