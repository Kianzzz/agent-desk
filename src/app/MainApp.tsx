import { BarChart3, Blocks, HardDrive, LayoutDashboard, Settings as SettingsIcon, ShieldCheck } from "lucide-react";
import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { inTauri, onEvent } from "../api";
import { bytes } from "../lib/format";
import { useDisk, useSecurity, useSettings } from "../lib/hooks";
import { sumSafe } from "../lib/disk";
import { Overview } from "../pages/Overview";
import { Usage } from "../pages/Usage";
import { Extensions } from "../pages/Extensions";
import { Security } from "../pages/Security";
import { Cleanup } from "../pages/Cleanup";
import { SettingsPage } from "../pages/Settings";
import { Logo } from "../components/Logo";

export type Page = "overview" | "usage" | "extensions" | "security" | "cleanup" | "settings";

const NAV: { id: Page; label: string; Icon: typeof BarChart3 }[] = [
  { id: "overview", label: "概览", Icon: LayoutDashboard },
  { id: "usage", label: "用量", Icon: BarChart3 },
  { id: "extensions", label: "MCP · 技能 · 钩子", Icon: Blocks },
  { id: "security", label: "安全", Icon: ShieldCheck },
  { id: "cleanup", label: "空间清理", Icon: HardDrive },
  { id: "settings", label: "设置", Icon: SettingsIcon },
];

export function MainApp() {
  const [page, setPage] = useState<Page>(() => (localStorage.getItem("page") as Page) || "overview");
  const [security] = useSecurity();
  const [disk] = useDisk();
  const [settings] = useSettings();

  useEffect(() => {
    try {
      localStorage.setItem("page", page);
    } catch {
      /* 存不了就算了 */
    }
    document.querySelector(".main")?.scrollTo(0, 0);
  }, [page]);

  useEffect(() => onEvent<string>("navigate", (p) => NAV.some((n) => n.id === p) && setPage(p as Page)), []);

  useWindowDrag();

  const highs = security?.findings.filter((f) => f.severity === "high" && !settings?.ignoredFindings.includes(f.id)).length ?? 0;
  const safe = disk ? sumSafe(disk.locations) : 0;

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="sidebar-drag" />
        <div className="brand">
          <Logo size={18} />
          Agent Desk
        </div>
        <nav className="nav">
          {NAV.map(({ id, label, Icon }) => (
            <button key={id} className={page === id ? "active" : ""} onClick={() => setPage(id)}>
              <Icon size={15} />
              {label}
              {id === "security" && highs > 0 && <span className="count alert">{highs}</span>}
              {id === "cleanup" && safe > 1e9 && <span className="count">{bytes(safe)}</span>}
            </button>
          ))}
        </nav>
        <div className="sidebar-foot">
          数据只在本机读取
          <br />
          删除只移到废纸篓
        </div>
      </aside>
      <main className="main">
        {page === "overview" && <Overview go={setPage} />}
        {page === "usage" && <Usage />}
        {page === "extensions" && <Extensions />}
        {page === "security" && <Security />}
        {page === "cleanup" && <Cleanup />}
        {page === "settings" && <SettingsPage />}
      </main>
    </div>
  );
}

/** 页头和左侧导航的空白处都能拖动窗口，双击放大或还原（跟系统标题栏一样）。
 *  按钮、开关、输入框这些可点的东西照常响应点击。 */
const DRAG_AREAS = ".page-head, .sidebar";
const INTERACTIVE = "button, a, input, select, textarea, label, [role=button], [role=switch], [role=tab], [role=tablist], [role=radio]";

function useWindowDrag() {
  useEffect(() => {
    if (!inTauri) return;
    const onDown = (e: MouseEvent) => {
      if (e.button !== 0) return;
      const t = e.target as HTMLElement | null;
      if (!t?.closest(DRAG_AREAS) || t.closest(INTERACTIVE)) return;
      const win = getCurrentWindow();
      if (e.detail === 2) win.toggleMaximize().catch(() => {});
      else win.startDragging().catch(() => {});
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, []);
}
