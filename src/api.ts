import type {
  Account,
  AssessInput,
  BridgeStatus,
  Assessment,
  DiskReport,
  Inventory,
  ScanReport,
  Settings,
  TrashResult,
  UsageState,
} from "./types";

/** 在 Tauri 里走 invoke；在普通浏览器里（开发预览）走假数据。 */
export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (inTauri) {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<T>(cmd, args);
  }
  const { mockInvoke } = await import("./mock");
  return mockInvoke(cmd, args) as Promise<T>;
}

export const api = {
  getUsage: () => call<UsageState>("get_usage"),
  refreshUsage: () => call<UsageState>("refresh_usage"),
  updatePricing: () => call<number>("update_pricing"),

  getAccounts: () => call<Account[]>("get_accounts"),
  claudeBridgeStatus: () => call<BridgeStatus>("claude_bridge_status"),
  setClaudeBridge: (enabled: boolean) => call<BridgeStatus>("set_claude_bridge", { enabled }),

  getInventory: () => call<Inventory>("get_inventory"),
  setEnabled: (kind: "skill" | "mcp" | "hook", id: string, enabled: boolean) =>
    call<void>("set_enabled", { kind, id, enabled }),
  trashSkill: (id: string) => call<void>("trash_skill", { id }),
  readSkillMd: (path: string) => call<string>("read_skill_md", { path }),

  getSecurity: () => call<ScanReport | null>("get_security"),
  runSecurityScan: () => call<ScanReport>("run_security_scan"),

  getDisk: () => call<DiskReport | null>("get_disk"),
  runDiskScan: (includeProjects: boolean) => call<DiskReport>("run_disk_scan", { includeProjects }),
  assessItems: (items: AssessInput[]) => call<Assessment[]>("assess_items", { items }),
  trashPaths: (paths: string[]) => call<TrashResult[]>("trash_paths", { paths }),

  getSettings: () => call<Settings>("get_settings"),
  saveSettings: (settings: Settings) => call<Settings>("save_settings", { settings }),

  openMain: (page?: string) => call<void>("open_main", { page: page ?? null }),
  hideTrayPanel: () => call<void>("hide_tray_panel"),
  setPanelHeight: (label: string, height: number) => call<void>("set_panel_height", { label, height }),
  revealPath: (path: string) => call<void>("reveal_path", { path }),
  openPath: (path: string) => call<void>("open_path", { path }),
  quit: () => call<void>("quit_app"),
};

/** 订阅后端事件；浏览器预览里什么也不做。返回取消函数。 */
export function onEvent<T = unknown>(name: string, handler: (payload: T) => void): () => void {
  if (!inTauri) return () => {};
  let unlisten: (() => void) | null = null;
  let cancelled = false;
  import("@tauri-apps/api/event").then(({ listen }) =>
    listen<T>(name, (e) => handler(e.payload)).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    }),
  );
  return () => {
    cancelled = true;
    unlisten?.();
  };
}

export async function windowLabel(): Promise<string> {
  const q = new URLSearchParams(location.search).get("w");
  if (q) return q;
  if (!inTauri) return "main";
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  return getCurrentWindow().label;
}
