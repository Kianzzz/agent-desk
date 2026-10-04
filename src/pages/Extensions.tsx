import { Copy, ExternalLink, FileText, FolderOpen, Link2, RefreshCw, Search, Terminal, Trash2, X, AlertTriangle } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { api } from "../api";
import { Confirm, Empty, ErrorBox, Loading, Toast, Toggle, useToast } from "../components/ui";
import { ago, basename, bytes, tildify } from "../lib/format";
import { useAction } from "../lib/hooks";
import type { Client, HookEntry, Inventory, McpServer, SkillEntry } from "../types";

const CLIENT_LABEL: Record<Client, string> = {
  claudeCode: "Claude Code",
  claudeDesktop: "Claude 桌面版",
  codex: "Codex",
  gemini: "Gemini CLI",
  cursor: "Cursor",
};

const SCOPE_LABEL: Record<string, string> = {
  user: "全局",
  local: "单个项目（私有）",
  project: "项目共享",
  plugin: "插件",
};

type Tab = "mcp" | "skills" | "hooks" | "plugins";

export function Extensions() {
  const [inv, setInv] = useState<Inventory | null>(null);
  const [tab, setTab] = useState<Tab>(() => (sessionStorage.getItem("ext-tab") as Tab) || "skills");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const toast = useToast();

  const load = async () => {
    setLoading(true);
    try {
      setInv(await api.getInventory());
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };
  useEffect(() => {
    load();
  }, []);
  useEffect(() => {
    try {
      sessionStorage.setItem("ext-tab", tab);
    } catch {
      /* 忽略 */
    }
  }, [tab]);

  const toggle = async (kind: "skill" | "mcp" | "hook", id: string, enabled: boolean, name: string) => {
    try {
      await api.setEnabled(kind, id, enabled);
      toast.show(`已${enabled ? "启用" : "停用"}「${name}」${kind === "skill" ? "" : "，重启对应客户端后生效"}`);
      await load();
    } catch (e) {
      toast.show(String(e));
    }
  };

  return (
    <div className="page">
      <div className="page-head">
        <div>
          <h1>MCP · 技能 · 钩子</h1>
          <div className="sub">停用不会删除：配置存进 ~/.agent-desk，随时可以重新启用。修改配置前会自动备份。</div>
        </div>
        <div className="actions">
          {inv && <span className="muted" style={{ fontSize: 12 }}>扫描于 {ago(inv.scannedAt)}</span>}
          <button className="btn" onClick={load} disabled={loading}>
            <RefreshCw size={13} className={loading ? "spin" : ""} /> 重新扫描
          </button>
        </div>
      </div>
      <ErrorBox error={error} />
      {!inv ? (
        !error && <Loading />
      ) : (
        <>
          <div className="tabs">
            {(
              [
                ["skills", "技能", inv.skills.length],
                ["mcp", "MCP 服务", inv.mcp.length],
                ["hooks", "钩子", inv.hooks.length],
                ["plugins", "插件", inv.plugins.length],
              ] as [Tab, string, number][]
            ).map(([id, label, n]) => (
              <button key={id} className={tab === id ? "on" : ""} onClick={() => setTab(id)}>
                {label} <span className="n">{n}</span>
              </button>
            ))}
          </div>
          {inv.warnings.length > 0 && (
            <div className="note" style={{ marginBottom: 12 }}>
              <AlertTriangle size={14} />
              <div>
                {inv.warnings.map((w, i) => (
                  <div key={i}>{w}</div>
                ))}
              </div>
            </div>
          )}
          {tab === "skills" && <Skills inv={inv} onToggle={toggle} onChanged={load} toast={toast.show} />}
          {tab === "mcp" && <Mcp inv={inv} onToggle={toggle} />}
          {tab === "hooks" && <Hooks hooks={inv.hooks} onToggle={toggle} />}
          {tab === "plugins" && <Plugins inv={inv} />}
        </>
      )}
      <Toast text={toast.text} onDone={toast.clear} />
    </div>
  );
}

type ToggleFn = (kind: "skill" | "mcp" | "hook", id: string, enabled: boolean, name: string) => void;

// ---------- 技能 ----------

function Skills({ inv, onToggle, onChanged, toast }: { inv: Inventory; onToggle: ToggleFn; onChanged: () => void; toast: (s: string) => void }) {
  const [q, setQ] = useState("");
  const [root, setRoot] = useState<string | null>(null);
  const [onlyDup, setOnlyDup] = useState(false);
  const [preview, setPreview] = useState<SkillEntry | null>(null);
  const [trashing, setTrashing] = useState<SkillEntry | null>(null);
  const trash = useAction(api.trashSkill);

  const dupNames = useMemo(() => new Map(inv.skillGroups.map((g) => [g.name, g])), [inv]);
  const conflicting = inv.skillGroups.filter((g) => !g.identical);

  const list = inv.skills
    .filter((s) => !root || s.rootId === root)
    .filter((s) => !onlyDup || dupNames.has(s.name))
    .filter((s) => {
      if (!q) return true;
      const k = q.toLowerCase();
      return s.name.toLowerCase().includes(k) || s.description.toLowerCase().includes(k);
    })
    .sort((a, b) => a.name.localeCompare(b.name) || a.rootId.localeCompare(b.rootId));

  return (
    <>
      <div className="toolbar">
        <div className="search">
          <Search size={13} />
          <input className="input" placeholder="搜索名字或描述" value={q} onChange={(e) => setQ(e.target.value)} />
        </div>
        <div className="chips">
          <button className={"chip" + (root === null ? " on" : "")} onClick={() => setRoot(null)}>
            全部 <span className="n">{inv.skills.length}</span>
          </button>
          {inv.skillRoots
            .filter((r) => r.exists && r.count > 0)
            .map((r) => (
              <button key={r.id} className={"chip" + (root === r.id ? " on" : "")} onClick={() => setRoot(root === r.id ? null : r.id)} title={tildify(r.path)}>
                {r.label} <span className="n">{r.count}</span>
              </button>
            ))}
        </div>
        <span className="spacer" />
        <label style={{ display: "inline-flex", alignItems: "center", gap: 6, fontSize: 12 }}>
          <Toggle on={onlyDup} onChange={setOnlyDup} /> 只看重复的
        </label>
      </div>

      {inv.skillGroups.length > 0 && (
        <div className="note" style={{ marginBottom: 12 }}>
          <Copy size={14} />
          <span>
            有 {inv.skillGroups.length} 个技能同时出现在多个位置，其中 {inv.skillGroups.length - conflicting.length} 个内容一致（多半是符号链接，正常）
            {conflicting.length > 0 && (
              <>
                ，<b style={{ color: "var(--serious-ink)" }}>{conflicting.length} 个内容不一致</b>：{conflicting.slice(0, 6).map((g) => g.name).join("、")}
                {conflicting.length > 6 ? " 等" : ""}。不一致时不同客户端用到的是不同版本。
              </>
            )}
          </span>
        </div>
      )}

      <div className="card">
        {list.length === 0 ? (
          <Empty title="没有符合条件的技能" />
        ) : (
          <div className="list">
            {list.map((s) => {
              const g = dupNames.get(s.name);
              return (
                <div className={"row" + (s.enabled ? "" : " disabled")} key={s.id}>
                  <div className="row-main">
                    <div className="row-title">
                      <span className="truncate" title={s.dirName !== s.name ? `目录名：${s.dirName}` : undefined}>
                        {s.name}
                      </span>
                      <span className="badge">{s.rootLabel}</span>
                      {s.isSymlink && (
                        <span className="badge" title={`链接到 ${tildify(s.realPath)}`}>
                          <Link2 size={11} /> 链接
                        </span>
                      )}
                      {s.hasScripts && (
                        <span className="badge" title="目录里有可执行脚本">
                          <Terminal size={11} /> 脚本
                        </span>
                      )}
                      {g && (
                        <span className={"badge " + (g.identical ? "" : "serious")} title={`出现在 ${g.entryIds.length} 个位置`}>
                          {g.identical ? `${g.entryIds.length} 处一致` : `${g.entryIds.length} 处不一致`}
                        </span>
                      )}
                      {s.syncedBy && (
                        <span className="badge" title={`由 ${s.syncedBy} 同步进来；停用后 ${s.syncedBy} 下次同步可能会放回去`}>
                          {s.syncedBy} 同步
                        </span>
                      )}
                      {!s.enabled && <span className="badge warning">已停用</span>}
                    </div>
                    <div className="row-sub clamp2" title={s.description}>
                      {s.description || "（没有描述）"}
                    </div>
                  </div>
                  <div className="muted num" style={{ fontSize: 11.5, textAlign: "right", flex: "none", width: 92 }}>
                    {bytes(s.sizeBytes)}
                    <br />
                    {ago(s.modified)}
                  </div>
                  <div className="row-actions">
                    <button className="icon-btn" title="预览 SKILL.md" onClick={() => setPreview(s)}>
                      <FileText size={14} />
                    </button>
                    <button className="icon-btn" title="在访达中显示" onClick={() => api.revealPath(s.archivedPath ?? s.path)}>
                      <FolderOpen size={14} />
                    </button>
                    {s.manageable && (
                      <button className="icon-btn" title="移到废纸篓" onClick={() => setTrashing(s)}>
                        <Trash2 size={14} />
                      </button>
                    )}
                    <Toggle
                      on={s.enabled}
                      disabled={!s.manageable}
                      title={s.manageable ? (s.enabled ? "停用" : "启用") : "插件里的技能要在插件里管理"}
                      onChange={(v) => onToggle("skill", s.id, v, s.name)}
                    />
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>

      {preview && <SkillPreview skill={preview} onClose={() => setPreview(null)} />}
      {trashing && (
        <Confirm
          title={`把「${trashing.name}」移到废纸篓？`}
          confirmText="移到废纸篓"
          danger
          busy={trash.busy}
          onCancel={() => setTrashing(null)}
          onConfirm={async () => {
            const ok = await trash.run(trashing.id);
            if (ok !== undefined) {
              toast(`已把「${trashing.name}」移到废纸篓`);
              setTrashing(null);
              onChanged();
            }
          }}
        >
          <p className="secondary" style={{ margin: 0 }}>
            位置：<span className="mono">{tildify(trashing.path)}</span>
          </p>
          {trashing.isSymlink && <p className="secondary">这是一个链接，只删除链接本身，不动 {tildify(trashing.realPath)}。</p>}
          <p className="muted">可以在废纸篓里恢复。只是暂时不想用的话，用右边的开关停用更方便。</p>
          <ErrorBox error={trash.error} />
        </Confirm>
      )}
    </>
  );
}

function SkillPreview({ skill, onClose }: { skill: SkillEntry; onClose: () => void }) {
  const [text, setText] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    api.readSkillMd(skill.realPath).then(setText, (e) => setErr(String(e)));
  }, [skill.realPath]);
  return (
    <>
      <div className="scrim" onClick={onClose} style={{ background: "rgba(0,0,0,0.15)" }} />
      <div className="drawer">
        <div className="drawer-head">
          <div style={{ minWidth: 0 }}>
            <div style={{ fontWeight: 600 }}>{skill.name}</div>
            <div className="muted mono truncate">{tildify(skill.realPath)}/SKILL.md</div>
          </div>
          <button className="icon-btn" style={{ marginLeft: "auto" }} title="用默认程序打开" onClick={() => api.openPath(skill.realPath + "/SKILL.md")}>
            <ExternalLink size={14} />
          </button>
          <button className="icon-btn" onClick={onClose} title="关闭">
            <X size={15} />
          </button>
        </div>
        <div className="drawer-body">
          <ErrorBox error={err} />
          {text === null && !err ? <Loading /> : <pre className="code selectable" style={{ margin: 0 }}>{text}</pre>}
        </div>
      </div>
    </>
  );
}

// ---------- MCP ----------

function Mcp({ inv, onToggle }: { inv: Inventory; onToggle: ToggleFn }) {
  const clients = [...new Set(inv.mcp.map((m) => m.client))];
  if (inv.mcp.length === 0) return <div className="card"><Empty title="没有找到 MCP 服务" /></div>;
  return (
    <div className="card">
      {clients.map((c) => (
        <div key={c}>
          <div className="group-head">
            {CLIENT_LABEL[c]} <span style={{ fontWeight: 400 }}>· {inv.mcp.filter((m) => m.client === c).length} 个</span>
          </div>
          <div className="list">
            {inv.mcp
              .filter((m) => m.client === c)
              .map((m) => (
                <McpRow key={m.id} m={m} onToggle={onToggle} />
              ))}
          </div>
        </div>
      ))}
    </div>
  );
}

function McpRow({ m, onToggle }: { m: McpServer; onToggle: ToggleFn }) {
  const target = m.url ?? [m.command, ...m.args].filter(Boolean).join(" ");
  return (
    <div className={"row" + (m.enabled ? "" : " disabled")}>
      <div className="row-main">
        <div className="row-title">
          <span className="truncate">{m.name}</span>
          <span className="badge">{m.transport === "stdio" ? "本地进程" : m.transport.toUpperCase()}</span>
          <span className="badge" title={m.scopePath ?? undefined}>
            {SCOPE_LABEL[m.scope] ?? m.scope}
            {m.scopePath ? ` · ${basename(m.scopePath)}` : ""}
          </span>
          {!m.enabled && <span className="badge warning">已停用</span>}
        </div>
        <div className="row-sub mono truncate" title={target}>
          {target}
        </div>
        {(m.envKeys.length > 0 || m.headerKeys.length > 0) && (
          <div style={{ display: "flex", gap: 4, marginTop: 4, flexWrap: "wrap" }}>
            {[...m.envKeys, ...m.headerKeys].map((k) => (
              <span key={k} className="badge mono" style={{ fontSize: 10.5 }}>
                {k}
              </span>
            ))}
          </div>
        )}
      </div>
      <div className="row-actions">
        <button className="icon-btn" title={`在访达中显示 ${tildify(m.configPath)}`} onClick={() => api.revealPath(m.configPath)}>
          <FolderOpen size={14} />
        </button>
        <Toggle
          on={m.enabled}
          disabled={!m.manageable}
          title={m.manageable ? (m.enabled ? "停用" : "启用") : "项目或插件里的 MCP 要在对应位置修改"}
          onChange={(v) => onToggle("mcp", m.id, v, m.name)}
        />
      </div>
    </div>
  );
}

// ---------- 钩子 ----------

function Hooks({ hooks, onToggle }: { hooks: HookEntry[]; onToggle: ToggleFn }) {
  if (hooks.length === 0) {
    return (
      <div className="card">
        <Empty title="没有配置钩子">钩子是 AI 在特定时机（比如执行命令前、回答结束后）自动运行的命令。</Empty>
      </div>
    );
  }
  const events = [...new Set(hooks.map((h) => h.event))];
  return (
    <div className="card">
      {events.map((ev) => (
        <div key={ev}>
          <div className="group-head">{ev}</div>
          <div className="list">
            {hooks
              .filter((h) => h.event === ev)
              .map((h) => (
                <div className={"row" + (h.enabled ? "" : " disabled")} key={h.id}>
                  <div className="row-main">
                    <div className="row-title">
                      <span className="badge">{CLIENT_LABEL[h.client]}</span>
                      <span className="badge">
                        {SCOPE_LABEL[h.scope] ?? h.scope}
                        {h.scopePath ? ` · ${basename(h.scopePath)}` : ""}
                      </span>
                      {h.matcher && <span className="badge mono">匹配 {h.matcher}</span>}
                      {!h.enabled && <span className="badge warning">已停用</span>}
                    </div>
                    <div className="code selectable" style={{ marginTop: 6 }}>
                      {h.command}
                    </div>
                  </div>
                  <div className="row-actions">
                    <button className="icon-btn" title="在访达中显示配置文件" onClick={() => api.revealPath(h.configPath)}>
                      <FolderOpen size={14} />
                    </button>
                    <Toggle on={h.enabled} disabled={!h.manageable} onChange={(v) => onToggle("hook", h.id, v, h.event)} />
                  </div>
                </div>
              ))}
          </div>
        </div>
      ))}
    </div>
  );
}

// ---------- 插件 ----------

function Plugins({ inv }: { inv: Inventory }) {
  if (inv.plugins.length === 0) {
    return (
      <div className="card">
        <Empty title="没有安装 Claude Code 插件" />
      </div>
    );
  }
  return (
    <div className="card">
      <div className="list">
        {inv.plugins.map((p) => (
          <div className={"row" + (p.enabled ? "" : " disabled")} key={p.id}>
            <div className="row-main">
              <div className="row-title">
                {p.name}
                {p.version && <span className="badge mono">{p.version}</span>}
                {p.marketplace && <span className="badge">{p.marketplace}</span>}
                <span className={"badge " + (p.enabled ? "good" : "")}>{p.enabled ? "已启用" : "未启用"}</span>
              </div>
              <div className="row-sub">
                {[
                  p.skills && `${p.skills} 个技能`,
                  p.commands && `${p.commands} 个命令`,
                  p.agents && `${p.agents} 个子代理`,
                  p.mcpServers && `${p.mcpServers} 个 MCP`,
                  p.hooks && `${p.hooks} 个钩子`,
                ]
                  .filter(Boolean)
                  .join(" · ") || "没有可识别的内容"}
              </div>
            </div>
            <button className="icon-btn" title="在访达中显示" onClick={() => api.revealPath(p.path)}>
              <FolderOpen size={14} />
            </button>
          </div>
        ))}
      </div>
      <div className="muted card-pad" style={{ fontSize: 11.5 }}>
        插件的启用和卸载请在 Claude Code 里用 /plugin 管理。
      </div>
    </div>
  );
}

