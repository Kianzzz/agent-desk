import { Eye, EyeOff, ExternalLink, FolderOpen, RefreshCw, ShieldCheck } from "lucide-react";
import { useState } from "react";
import { api } from "../api";
import { Empty, ErrorBox, Loading, SeverityBadge, severityLabel } from "../components/ui";
import { ago, tildify } from "../lib/format";
import { useAction, useSecurity, useSettings } from "../lib/hooks";
import type { Category, Finding, Severity } from "../types";

const CATEGORY_LABEL: Record<Category, string> = {
  hiddenChars: "隐藏字符",
  promptInjection: "提示词注入",
  dangerousCommand: "危险命令",
  plaintextSecret: "明文密钥",
  broadPermission: "权限过宽",
  supplyChain: "依赖来源",
  insecureTransport: "明文连接",
  filePermission: "文件权限",
};

const KIND_LABEL: Record<string, string> = {
  skill: "技能",
  hook: "钩子",
  mcp: "MCP",
  settings: "设置",
  instructions: "说明文件",
  credentials: "凭据",
  shell: "Shell 配置",
};

const SEVERITIES: Severity[] = ["high", "medium", "low", "info"];

export function Security() {
  const [report, reload] = useSecurity();
  const [settings, reloadSettings] = useSettings();
  const scan = useAction(api.runSecurityScan);
  const [sev, setSev] = useState<Set<Severity>>(new Set());
  const [cat, setCat] = useState<Category | null>(null);
  const [showIgnored, setShowIgnored] = useState(false);

  const ignored = new Set(settings?.ignoredFindings ?? []);
  const setIgnored = async (id: string, on: boolean) => {
    if (!settings) return;
    const next = new Set(settings.ignoredFindings);
    if (on) next.add(id);
    else next.delete(id);
    await api.saveSettings({ ...settings, ignoredFindings: [...next] });
    reloadSettings();
  };

  const head = (
    <div className="page-head">
      <div>
        <h1>安全</h1>
        <div className="sub">只读检查 AI 的配置、技能、钩子、项目说明和 Shell 配置，不会修改任何文件。</div>
      </div>
      <div className="actions">
        {report && (
          <span className="muted" style={{ fontSize: 12 }}>
            {ago(report.scannedAt)}扫描了 {report.filesScanned} 个文件
          </span>
        )}
        <button
          className="btn"
          disabled={scan.busy}
          onClick={async () => {
            await scan.run();
            reload();
          }}
        >
          <RefreshCw size={13} className={scan.busy ? "spin" : ""} /> 重新扫描
        </button>
      </div>
    </div>
  );

  if (!report) {
    return (
      <div className="page">
        {head}
        <Loading text="正在扫描…" />
      </div>
    );
  }

  const visible = report.findings.filter((f) => showIgnored || !ignored.has(f.id));
  const counts = (s: Severity) => visible.filter((f) => f.severity === s).length;
  const cats = [...new Set(visible.map((f) => f.category))];
  const list = visible.filter((f) => (sev.size === 0 || sev.has(f.severity)) && (!cat || f.category === cat));
  const ignoredCount = report.findings.filter((f) => ignored.has(f.id)).length;

  return (
    <div className="page">
      {head}
      <ErrorBox error={scan.error} />

      <div className="grid cols-4">
        {SEVERITIES.map((s) => (
          <button
            key={s}
            className="card card-pad"
            style={{ textAlign: "left", cursor: "pointer", outline: sev.has(s) ? "2px solid var(--accent)" : undefined }}
            onClick={() => {
              const n = new Set(sev);
              if (n.has(s)) n.delete(s);
              else n.add(s);
              setSev(n);
            }}
          >
            <SeverityBadge s={s} />
            <div className="stat-value" style={{ marginTop: 6 }}>
              {counts(s)}
            </div>
            <div className="stat-sub">{hint(s)}</div>
          </button>
        ))}
      </div>

      <div className="toolbar section">
        <div className="chips">
          <button className={"chip" + (cat === null ? " on" : "")} onClick={() => setCat(null)}>
            全部类别
          </button>
          {cats.map((c) => (
            <button key={c} className={"chip" + (cat === c ? " on" : "")} onClick={() => setCat(cat === c ? null : c)}>
              {CATEGORY_LABEL[c]} <span className="n">{visible.filter((f) => f.category === c).length}</span>
            </button>
          ))}
        </div>
        <span className="spacer" />
        {ignoredCount > 0 && (
          <button className="btn ghost sm" onClick={() => setShowIgnored(!showIgnored)}>
            {showIgnored ? <EyeOff size={12} /> : <Eye size={12} />}
            {showIgnored ? "隐藏已忽略" : `显示已忽略（${ignoredCount}）`}
          </button>
        )}
      </div>

      {list.length === 0 ? (
        <div className="card">
          <Empty title={visible.length === 0 ? "没有发现问题" : "没有符合筛选条件的项"} icon={<ShieldCheck size={22} />} />
        </div>
      ) : (
        <div className="card">
          {SEVERITIES.filter((s) => list.some((f) => f.severity === s)).map((s) => (
            <div key={s}>
              <div className="group-head">
                {severityLabel(s)} · {list.filter((f) => f.severity === s).length}
              </div>
              {list
                .filter((f) => f.severity === s)
                .map((f) => (
                  <FindingRow key={f.id} f={f} ignored={ignored.has(f.id)} onIgnore={(on) => setIgnored(f.id, on)} />
                ))}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function hint(s: Severity) {
  switch (s) {
    case "high":
      return "建议尽快处理";
    case "medium":
      return "值得看一下";
    case "low":
      return "了解即可";
    default:
      return "参考信息";
  }
}

function FindingRow({ f, ignored, onIgnore }: { f: Finding; ignored: boolean; onIgnore: (on: boolean) => void }) {
  const [open, setOpen] = useState(f.severity === "high");
  const loc = tildify(f.path) + (f.line ? `:${f.line}` : "");
  return (
    <div className={"row" + (ignored ? " disabled" : "")} style={{ alignItems: "flex-start", cursor: "pointer" }} onClick={() => setOpen(!open)}>
      <div className="row-main">
        <div className="row-title" style={{ flexWrap: "wrap" }}>
          <span>{f.title}</span>
          <span className="badge">{CATEGORY_LABEL[f.category]}</span>
          <span className="badge">
            {KIND_LABEL[f.targetKind] ?? f.targetKind}
            {f.targetName ? ` · ${f.targetName}` : ""}
          </span>
          {ignored && <span className="badge">已忽略</span>}
        </div>
        <div className="row-sub mono truncate">{loc}</div>
        {open && (
          <div style={{ marginTop: 8 }} onClick={(e) => e.stopPropagation()}>
            {f.excerpt && <div className="code selectable">{f.excerpt}</div>}
            <div className="secondary selectable" style={{ marginTop: 8, fontSize: 12.5, whiteSpace: "pre-wrap" }}>
              {f.detail}
            </div>
          </div>
        )}
      </div>
      <div className="row-actions" onClick={(e) => e.stopPropagation()}>
        <button className="icon-btn" title="在访达中显示" onClick={() => api.revealPath(f.path)}>
          <FolderOpen size={14} />
        </button>
        <button className="icon-btn" title="用默认程序打开" onClick={() => api.openPath(f.path)}>
          <ExternalLink size={14} />
        </button>
        <button className="btn ghost sm" onClick={() => onIgnore(!ignored)}>
          {ignored ? "取消忽略" : "忽略"}
        </button>
      </div>
    </div>
  );
}
