import { FolderOpen, Power } from "lucide-react";
import { useEffect, useState } from "react";
import { api, inTauri } from "../api";
import { ErrorBox, Loading, Segmented, Spinner, Toggle } from "../components/ui";
import { useSettings } from "../lib/hooks";
import type { BridgeStatus, Settings, Theme } from "../types";

export function SettingsPage() {
  const [saved, reload] = useSettings();
  const [s, setS] = useState<Settings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [bridge, setBridge] = useState<BridgeStatus | null>(null);
  const [bridgeBusy, setBridgeBusy] = useState(false);
  const [version, setVersion] = useState("");

  useEffect(() => {
    if (saved) setS(saved);
  }, [saved]);
  useEffect(() => {
    api.claudeBridgeStatus().then(setBridge).catch(() => setBridge(null));
    if (inTauri) {
      import("@tauri-apps/api/app").then(({ getVersion }) => getVersion().then(setVersion)).catch(() => {});
    }
  }, []);

  if (!s) {
    return (
      <div className="page">
        <div className="page-head">
          <h1>设置</h1>
        </div>
        <Loading />
      </div>
    );
  }

  const save = async (next: Settings) => {
    setS(next);
    try {
      await api.saveSettings(next);
      setError(null);
      reload();
    } catch (e) {
      setError(String(e));
    }
  };
  const set = <K extends keyof Settings>(k: K, v: Settings[K]) => save({ ...s, [k]: v });

  const toggleBridge = async (on: boolean) => {
    setBridgeBusy(true);
    setError(null);
    try {
      setBridge(await api.setClaudeBridge(on));
    } catch (e) {
      setError(String(e));
    } finally {
      setBridgeBusy(false);
    }
  };

  return (
    <div className="page" style={{ maxWidth: 760 }}>
      <div className="page-head">
        <h1>设置</h1>
      </div>
      <ErrorBox error={error} />

      <Group title="外观">
        <Item label="主题" desc="主面板、边缘卡片和菜单栏卡片一起切换。">
          <Segmented<Theme>
            value={s.theme ?? "dark"}
            onChange={(v) => set("theme", v)}
            options={[
              { value: "system", label: "跟随系统" },
              { value: "light", label: "浅色" },
              { value: "dark", label: "深色" },
            ]}
          />
        </Item>
      </Group>

      <Group title="快速查看">
        <Item label="屏幕边缘细条" desc="鼠标贴到屏幕边缘弹出今天的用量和额度；额度用到 70% 细条变黄，90% 变红。">
          <Toggle on={s.edgeEnabled} onChange={(v) => set("edgeEnabled", v)} />
        </Item>
        {s.edgeEnabled && (
          <Item label="细条位置">
            <Segmented
              value={s.edgeSide}
              onChange={(v) => set("edgeSide", v)}
              options={[
                { value: "left", label: "左边" },
                { value: "right", label: "右边" },
              ]}
            />
          </Item>
        )}
        <Item label="菜单栏图标" desc="点图标弹出同样的卡片，右键有菜单。">
          <Toggle on={s.trayEnabled} onChange={(v) => set("trayEnabled", v)} />
        </Item>
        {s.trayEnabled && (
          <Item label="图标旁显示今天的 tokens">
            <Toggle on={s.trayShowCost} onChange={(v) => set("trayShowCost", v)} />
          </Item>
        )}
      </Group>

      <Group title="额度数据">
        <Item
          label="Claude 额度"
          desc={
            bridge === "installed"
              ? "已接入 Claude Code 状态栏。在终端里用 Claude Code 时，5 小时和每周额度会自动更新；你原来的状态栏显示不变。断开会把原来的设置放回去。"
              : bridge === "broken"
                ? "状态栏设置指向 Agent Desk，但脚本不见了，重新打开开关即可修复。"
                : "接入后从 Claude Code 状态栏读取 5 小时和每周额度。会修改 ~/.claude/settings.json 的 statusLine（改前自动备份），原来的状态栏照常显示。"
          }
        >
          {bridgeBusy ? <Spinner size={14} /> : <Toggle on={bridge === "installed"} onChange={toggleBridge} />}
        </Item>
        <Item label="Codex 额度" desc="从 Codex 的会话日志读取，不需要设置。每次用 Codex 后更新。">
          <span className="label">自动</span>
        </Item>
        <Item label="Gemini 额度" desc="Gemini CLI 不在本机记录账号等级和额度，只显示登录方式和 token。">
          <span className="label">不可用</span>
        </Item>
      </Group>

      <Group title="运行">
        <Item label="开机自动启动">
          <Toggle on={s.launchAtLogin} onChange={(v) => set("launchAtLogin", v)} />
        </Item>
        <Item label="用量刷新间隔" desc="后台增量读取日志，没变化的文件不会重读。">
          <Segmented
            value={String(s.refreshSeconds)}
            onChange={(v) => set("refreshSeconds", Number(v))}
            options={[
              { value: "30", label: "30 秒" },
              { value: "60", label: "1 分钟" },
              { value: "300", label: "5 分钟" },
            ]}
          />
        </Item>
        <Item label="数据目录" desc="缓存、额度记录、停用的技能和 MCP、配置备份都在 ~/.agent-desk。">
          <button className="btn sm" onClick={() => api.openPath("~/.agent-desk")}>
            <FolderOpen size={12} /> 打开
          </button>
        </Item>
      </Group>

      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginTop: 24 }}>
        <span className="label">Agent Desk {version} · 所有数据只在本机处理</span>
        <button className="btn" onClick={() => api.quit()}>
          <Power size={12} /> 退出 Agent Desk
        </button>
      </div>
    </div>
  );
}

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="section">
      <div className="section-title">{title}</div>
      <div className="card">{children}</div>
    </div>
  );
}

function Item({ label, desc, children }: { label: string; desc?: string; children: React.ReactNode }) {
  return (
    <div className="row" style={{ padding: "12px 16px" }}>
      <div className="row-main">
        <div>{label}</div>
        {desc && (
          <div className="row-sub" style={{ lineHeight: 1.6 }}>
            {desc}
          </div>
        )}
      </div>
      {children}
    </div>
  );
}
