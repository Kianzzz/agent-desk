import { ChevronDown, ChevronRight, FolderOpen, RefreshCw, Sparkles, Trash2, FolderSearch, FolderX, X } from "lucide-react";
import { useMemo, useRef, useState } from "react";
import { api } from "../api";
import { Confirm, ErrorBox, Loading, SafetyBadge, safetyLabel, Segmented, Spinner, Toast, useToast } from "../components/ui";
import { ago, bytes, daysSince, tildify } from "../lib/format";
import { useAction, useDisk } from "../lib/hooks";
import type { Assessment, AssessInput, DiskItem, DiskReport, ProjectUsage, Safety } from "../types";

type View = "location" | "project";
/** 顶部三个数字块对应的筛选；all 表示不筛。 */
type Filter = "all" | "safe" | "review" | "keep";

const deletable = (it: DiskItem) => it.safety !== "protected" && !it.inUse;

/** 叶子项归到哪个数字块：正在写入和受保护的都算「建议保留」，三块加起来正好是全部。 */
function bucket(it: DiskItem): Exclude<Filter, "all"> {
  if (it.inUse || it.safety === "keep" || it.safety === "protected") return "keep";
  return it.safety;
}

const matches = (it: DiskItem, f: Filter) => f === "all" || bucket(it) === f;

/** 只保留符合筛选的叶子；目录的大小改成剩下叶子的合计，没剩下的目录整个去掉。 */
function prune(it: DiskItem, f: Filter): DiskItem | null {
  if (f === "all") return it;
  if (!it.children.length) return matches(it, f) ? it : null;
  const children = it.children.map((c) => prune(c, f)).filter((c): c is DiskItem => c !== null);
  if (!children.length) return null;
  return { ...it, children, sizeBytes: children.reduce((a, c) => a + c.sizeBytes, 0) };
}

function pruneAll(items: DiskItem[], f: Filter): DiskItem[] {
  if (f === "all") return items;
  return items
    .map((it) => prune(it, f))
    .filter((it): it is DiskItem => it !== null)
    .sort((a, b) => b.sizeBytes - a.sizeBytes);
}

/** 叶子节点（真正可以勾选删除的项）。 */
function leaves(items: DiskItem[]): DiskItem[] {
  return items.flatMap((it) => (it.children.length ? leaves(it.children) : [it]));
}

function allItems(report: DiskReport): DiskItem[] {
  const fromProjects = report.projects.flatMap((p) => [...p.transcripts, ...leaves(p.artifacts)]);
  return [...leaves(report.locations), ...fromProjects];
}

export function Cleanup() {
  const [report, reload] = useDisk();
  const [view, setView] = useState<View>("location");
  const [filter, setFilter] = useState<Filter>("all");
  const [selected, setSelected] = useState<Map<string, DiskItem>>(new Map());
  const [verdicts, setVerdicts] = useState<Map<string, Assessment>>(new Map());
  const [confirming, setConfirming] = useState(false);
  const toast = useToast();
  const scan = useAction(api.runDiskScan);
  const assess = useAction(api.assessItems);
  const trash = useAction(api.trashPaths);

  const index = useMemo(() => {
    const m = new Map<string, DiskItem>();
    if (report) allItems(report).forEach((it) => m.set(it.id, it));
    return m;
  }, [report]);

  const projectExists = useMemo(() => {
    const m = new Map<string, boolean>();
    report?.projects.forEach((p) => m.set(p.project, p.exists));
    return m;
  }, [report]);

  // 上一次点过的勾选框，按住 Shift 再点另一个时，把两者之间（按界面上的顺序）一起选上或取消
  const lastClicked = useRef<string | null>(null);

  const toggle = (it: DiskItem, shift = false) => {
    if (!deletable(it)) return;
    const next = new Map(selected);
    const want = !next.has(it.id);
    const anchor = lastClicked.current;
    lastClicked.current = it.id;
    if (shift && anchor && anchor !== it.id) {
      // 只看当前展开、显示在界面上的勾选框，顺序就是用户看到的顺序
      const ids = [...document.querySelectorAll<HTMLInputElement>("input[data-item-id]")].map((el) => el.dataset.itemId!);
      const a = ids.indexOf(anchor);
      const b = ids.indexOf(it.id);
      if (a >= 0 && b >= 0) {
        for (const id of ids.slice(Math.min(a, b), Math.max(a, b) + 1)) {
          const x = index.get(id);
          if (!x || !deletable(x)) continue;
          if (want) next.set(id, x);
          else next.delete(id);
        }
        setSelected(next);
        return;
      }
    }
    if (want) next.set(it.id, it);
    else next.delete(it.id);
    setSelected(next);
  };

  const toggleMany = (items: DiskItem[], on: boolean) => {
    const next = new Map(selected);
    items.filter(deletable).forEach((it) => (on ? next.set(it.id, it) : next.delete(it.id)));
    setSelected(next);
  };

  const selectedBytes = [...selected.values()].reduce((a, it) => a + it.sizeBytes, 0);

  const toInput = (it: DiskItem): AssessInput => ({
    id: it.id,
    path: it.path,
    label: it.label,
    category: it.category,
    project: it.project,
    projectExists: it.project ? (projectExists.get(it.project) ?? null) : null,
    sizeBytes: it.sizeBytes,
    modified: it.modified,
    title: it.title,
    safety: it.safety,
    reason: it.reason,
  });

  const runAssess = async (items: DiskItem[]) => {
    const res = await assess.run(items.slice(0, 120).map(toInput));
    if (!res) return;
    const next = new Map(verdicts);
    res.forEach((a) => next.set(a.id, a));
    setVerdicts(next);
    const del = res.filter((a) => a.verdict === "delete").length;
    toast.show(`AI 看了 ${res.length} 项：建议删 ${del} 项，其余建议保留或再看看`);
  };

  const reviewCandidates = () => {
    if (!report) return [];
    return [...index.values()]
      .filter((it) => it.safety === "review" && !it.inUse)
      .sort((a, b) => b.sizeBytes - a.sizeBytes)
      .slice(0, 50);
  };

  const pickAiDeletes = () => {
    const next = new Map(selected);
    verdicts.forEach((v, id) => {
      const it = index.get(id);
      if (v.verdict === "delete" && it && deletable(it)) next.set(id, it);
    });
    setSelected(next);
  };

  const doTrash = async () => {
    // 同时选了目录和它里面的文件时，只删目录
    const paths = [...selected.values()].map((it) => it.path);
    const top = paths.filter((p) => !paths.some((q) => q !== p && p.startsWith(q.replace(/\/$/, "") + "/")));
    const res = await trash.run(top);
    if (!res) return;
    const ok = res.filter((r) => r.ok);
    const freed = ok.reduce((a, r) => a + r.freedBytes, 0);
    const failed = res.filter((r) => !r.ok);
    setConfirming(false);
    setSelected(new Map());
    toast.show(
      `已移到废纸篓 ${ok.length} 项，腾出 ${bytes(freed)}` +
        (failed.length ? `；${failed.length} 项没删：${failed[0].error ?? ""}` : "") +
        "。清空废纸篓后空间才会真正释放。",
    );
    reload();
  };

  const head = (
    <div className="page-head">
      <div>
        <h1>空间清理</h1>
        <div className="sub">AI 工具留下的对话记录、缓存、临时工作区和构建产物。删除一律移到废纸篓。</div>
      </div>
      <div className="actions">
        {report && (
          <span className="muted" style={{ fontSize: 12 }}>
            {ago(report.scannedAt)}扫描
          </span>
        )}
        <button
          className="btn"
          disabled={scan.busy}
          title="连项目文件夹里的 node_modules、target 等一起统计，大约一分钟"
          onClick={async () => {
            await scan.run(true);
            reload();
            setView("project");
          }}
        >
          <FolderSearch size={13} /> 扫描项目文件夹
        </button>
        <button
          className="btn"
          disabled={scan.busy}
          onClick={async () => {
            await scan.run(false);
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
        <ErrorBox error={scan.error} />
        <Loading text={scan.busy ? "正在扫描…" : "启动后会自动扫描一次，稍等…"} />
      </div>
    );
  }

  const byBucket = (f: Exclude<Filter, "all">) => leaves(report.locations).filter((it) => bucket(it) === f).reduce((a, it) => a + it.sizeBytes, 0);
  // 再点一次已选中的块，回到全部
  const pick = (f: Filter) => setFilter(filter === f || f === "all" ? "all" : f);
  const locations = pruneAll(report.locations, filter);
  const projects =
    filter === "all"
      ? report.projects
      : report.projects.filter((p) => [...leaves(p.artifacts), ...p.transcripts].some((it) => matches(it, filter)));
  const usedPct = ((report.diskTotalBytes - report.diskFreeBytes) / report.diskTotalBytes) * 100;
  const scannedProjects = report.projects.some((p) => p.folderBytes !== null);

  return (
    <div className="page">
      {head}
      <ErrorBox error={scan.error} />

      <div className="grid cols-4">
        <button className={"card card-pad stat-btn" + (filter === "all" ? " active" : "")} aria-pressed={filter === "all"} title="显示全部" onClick={() => pick("all")}>
          <div className="stat-label">AI 工具共占用</div>
          <div className="stat-value">{bytes(report.totalAiBytes)}</div>
          <div className="stat-sub">
            磁盘已用 {usedPct.toFixed(0)}%，剩余 {bytes(report.diskFreeBytes)}
          </div>
        </button>
        <SafetyStat s="safe" v={byBucket("safe")} sub="缓存和能重新生成的东西" active={filter === "safe"} onClick={() => pick("safe")} />
        <SafetyStat s="review" v={byBucket("review")} sub="对话记录、生成的图片等" active={filter === "review"} onClick={() => pick("review")} />
        <SafetyStat s="keep" v={byBucket("keep")} sub="最近在用的、数据库和登录信息" active={filter === "keep"} onClick={() => pick("keep")} />
      </div>

      <div className="toolbar section">
        <Segmented<View>
          value={view}
          onChange={setView}
          options={[
            { value: "location", label: "按位置" },
            { value: "project", label: `按项目（${projects.length}）` },
          ]}
        />
        {filter !== "all" && (
          <span className="filter-chip">
            只看「{safetyLabel(filter)}」
            <button className="icon-btn" title="显示全部" onClick={() => setFilter("all")}>
              <X size={12} />
            </button>
          </span>
        )}
        <span className="spacer" />
        <button className="btn" disabled={assess.busy} onClick={() => runAssess(selected.size ? [...selected.values()] : reviewCandidates())} title="用你本机的 Claude 订阅（haiku 模型）判断，只发送路径、大小、时间和标题，不发送文件内容">
          {assess.busy ? <Spinner size={12} /> : <Sparkles size={13} />}
          {selected.size ? `让 AI 评估选中的 ${selected.size} 项` : "让 AI 评估「需确认」的大文件"}
        </button>
        {verdicts.size > 0 && (
          <button className="btn" onClick={pickAiDeletes}>
            勾选 AI 建议删的
          </button>
        )}
      </div>
      <ErrorBox error={assess.error} />

      {view === "location" ? (
        <div className="card">
          {locations.map((it) => (
            <TreeRow key={it.id} it={it} depth={0} selected={selected} verdicts={verdicts} onToggle={toggle} onToggleMany={toggleMany} max={locations[0]?.sizeBytes ?? 1} />
          ))}
          {locations.length === 0 && <div className="empty">没有「{safetyLabel(filter as Safety)}」的项</div>}
        </div>
      ) : (
        <>
          {!scannedProjects && (
            <div className="note" style={{ marginBottom: 12 }}>
              <FolderSearch size={14} />
              <span>现在只统计了对话记录。点右上角「扫描项目文件夹」，会把每个项目里的 node_modules、target、worktree 等也算进来。</span>
            </div>
          )}
          <div className="grid">
            {projects.map((p) => (
              <ProjectCard key={p.project} p={p} filter={filter} selected={selected} verdicts={verdicts} onToggle={toggle} onToggleMany={toggleMany} />
            ))}
            {projects.length === 0 && <div className="card empty">没有项目含「{safetyLabel(filter as Safety)}」的项</div>}
          </div>
        </>
      )}

      {selected.size > 0 && (
        <div className="sticky-bar">
          <span>
            已选 <b>{selected.size}</b> 项，共 <b>{bytes(selectedBytes)}</b>
          </span>
          <button className="btn ghost sm" onClick={() => setSelected(new Map())}>
            清除选择
          </button>
          <span className="label">按住 Shift 再点另一项，可以连选中间所有项</span>
          <span className="spacer" style={{ flex: 1 }} />
          <button className="btn danger" onClick={() => setConfirming(true)}>
            <Trash2 size={13} /> 移到废纸篓
          </button>
        </div>
      )}

      {confirming && (
        <Confirm
          title={`把 ${selected.size} 项（${bytes(selectedBytes)}）移到废纸篓？`}
          confirmText="移到废纸篓"
          danger
          busy={trash.busy}
          onCancel={() => setConfirming(false)}
          onConfirm={doTrash}
        >
          <div className="list" style={{ maxHeight: 280, overflow: "auto", border: "1px solid var(--border)", borderRadius: 8 }}>
            {[...selected.values()]
              .sort((a, b) => b.sizeBytes - a.sizeBytes)
              .map((it) => (
                <div className="row" key={it.id} style={{ padding: "6px 10px", minHeight: 0 }}>
                  <div className="row-main">
                    <div className="truncate" style={{ fontSize: 12.5 }}>
                      {it.title || it.label}
                    </div>
                    <div className="row-sub mono truncate">{tildify(it.path)}</div>
                  </div>
                  <span className="num muted">{bytes(it.sizeBytes)}</span>
                </div>
              ))}
          </div>
          <p className="muted" style={{ marginBottom: 0 }}>
            可以在废纸篓里恢复。删除前会先把对话记录里的用量收进 Agent Desk 的缓存，历史统计不受影响。正在写入的文件会被自动跳过。
          </p>
          <ErrorBox error={trash.error} />
        </Confirm>
      )}
      <Toast text={toast.text} onDone={toast.clear} />
    </div>
  );
}

function SafetyStat({ s, v, sub, active, onClick }: { s: Safety; v: number; sub: string; active: boolean; onClick: () => void }) {
  return (
    <button
      className={"card card-pad stat-btn" + (active ? " active" : "")}
      aria-pressed={active}
      title={active ? "再点一次显示全部" : `只看「${safetyLabel(s)}」的项`}
      onClick={onClick}
    >
      <SafetyBadge s={s} />
      <div className="stat-value" style={{ marginTop: 6 }}>
        {bytes(v)}
      </div>
      <div className="stat-sub">{sub}</div>
    </button>
  );
}

interface RowProps {
  selected: Map<string, DiskItem>;
  verdicts: Map<string, Assessment>;
  onToggle: (it: DiskItem, shift?: boolean) => void;
  onToggleMany: (items: DiskItem[], on: boolean) => void;
}

function Verdict({ v }: { v: Assessment | undefined }) {
  if (!v) return null;
  const cls = v.verdict === "delete" ? "good" : v.verdict === "keep" ? "accent" : "warning";
  const label = v.verdict === "delete" ? "AI：可以删" : v.verdict === "keep" ? "AI：留着" : "AI：再看看";
  return (
    <span className={"badge " + cls} title={v.reason}>
      <Sparkles size={11} />
      {label}
    </span>
  );
}

function Check({ it, selected, onToggle }: { it: DiskItem; selected: Map<string, DiskItem>; onToggle: (it: DiskItem, shift?: boolean) => void }) {
  const can = deletable(it);
  return (
    <input
      type="checkbox"
      checked={selected.has(it.id)}
      disabled={!can}
      title={can ? "" : it.inUse ? "正在写入，不能删" : "受保护，不能在这里删"}
      data-item-id={it.id}
      onChange={() => {}}
      onClick={(e) => {
        e.stopPropagation();
        onToggle(it, e.shiftKey);
      }}
      style={{ flex: "none" }}
    />
  );
}

/** 目录行的副标题：位置 + 里面各类可删空间。 */
function groupSummary(it: DiskItem): string {
  const ls = leaves(it.children);
  const sum = (f: (x: DiskItem) => boolean) => ls.filter(f).reduce((a, x) => a + x.sizeBytes, 0);
  const safe = sum((x) => x.safety === "safe" && !x.inUse);
  const review = sum((x) => x.safety === "review" && !x.inUse);
  const parts = [tildify(it.path)];
  if (safe > 0) parts.push(`可放心删 ${bytes(safe)}`);
  if (review > 0) parts.push(`需确认 ${bytes(review)}`);
  if (safe === 0 && review === 0) parts.push("里面没有可以删的项");
  return parts.join(" · ");
}

function TreeRow({ it, depth, max, ...p }: RowProps & { it: DiskItem; depth: number; max: number }) {
  const [open, setOpen] = useState(false);
  const has = it.children.length > 0;
  const v = p.verdicts.get(it.id);
  const leafs = has ? leaves(it.children).filter(deletable) : [];
  const allOn = has && leafs.length > 0 && leafs.every((l) => p.selected.has(l.id));
  return (
    <>
      <div className="row" style={{ paddingLeft: 16 + depth * 22, cursor: has ? "pointer" : "default" }} onClick={() => has && setOpen(!open)}>
        {has ? (
          <input
            type="checkbox"
            checked={allOn}
            disabled={leafs.length === 0}
            title={leafs.length ? "全选里面可以删的项" : "里面没有可以删的项"}
            onChange={() => p.onToggleMany(leafs, !allOn)}
            onClick={(e) => e.stopPropagation()}
          />
        ) : (
          <Check it={it} selected={p.selected} onToggle={p.onToggle} />
        )}
        <span style={{ width: 14, flex: "none", color: "var(--text-muted)" }}>{has && (open ? <ChevronDown size={14} /> : <ChevronRight size={14} />)}</span>
        <div className="row-main">
          <div className="row-title">
            <span className="truncate">{it.title || it.label}</span>
            {!has && <SafetyBadge s={it.safety} />}
            {!has && it.inUse && <span className="badge accent">正在使用</span>}
            <Verdict v={v} />
          </div>
          <div className="row-sub truncate" title={has ? it.reason : (v?.reason ?? it.reason)}>
            {has ? groupSummary(it) : v ? `AI：${v.reason}` : it.reason}
          </div>
        </div>
        <div style={{ width: 120, flex: "none" }}>
          <div className="num" style={{ textAlign: "right", fontWeight: 500 }}>
            {bytes(it.sizeBytes)}
          </div>
          {depth === 0 && (
            <div className="meter" style={{ height: 4, marginTop: 4 }}>
              <i style={{ width: `${(it.sizeBytes / max) * 100}%` }} />
            </div>
          )}
          {depth > 0 && <div className="muted num" style={{ fontSize: 11, textAlign: "right" }}>{ago(it.modified)}</div>}
        </div>
        <button
          className="icon-btn"
          title={`在访达中显示 ${tildify(it.path)}`}
          onClick={(e) => {
            e.stopPropagation();
            api.revealPath(it.path);
          }}
        >
          <FolderOpen size={14} />
        </button>
      </div>
      {open &&
        [...it.children]
          .sort((a, b) => b.sizeBytes - a.sizeBytes)
          .map((c) => <TreeRow key={c.id} it={c} depth={depth + 1} max={max} {...p} />)}
    </>
  );
}

function ProjectCard({ p, filter, ...rp }: RowProps & { p: ProjectUsage; filter: Filter }) {
  const [open, setOpen] = useState(false);
  const artifacts = pruneAll(p.artifacts, filter);
  const transcripts = p.transcripts.filter((it) => matches(it, filter));
  const items = [...leaves(artifacts), ...transcripts];
  const cleanable = items.filter(deletable);
  const safeBytes = items.filter((it) => bucket(it) === "safe").reduce((a, it) => a + it.sizeBytes, 0);
  const matchedBytes = items.reduce((a, it) => a + it.sizeBytes, 0);
  return (
    <div className="card">
      <div className="row" style={{ cursor: "pointer", borderBottom: open ? undefined : 0 }} onClick={() => setOpen(!open)}>
        <span style={{ color: "var(--text-muted)" }}>{open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}</span>
        <div className="row-main">
          <div className="row-title">
            <span className="truncate">{p.displayName}</span>
            {!p.exists && (
              <span className="badge warning">
                <FolderX size={11} /> 文件夹已不在
              </span>
            )}
            {p.tools.map((t) => (
              <span key={t} className="badge">
                {t === "claude" ? "Claude" : t === "codex" ? "Codex" : t === "gemini" ? "Gemini" : t}
              </span>
            ))}
          </div>
          <div className="row-sub truncate">
            {tildify(p.project)} · {p.sessions} 个对话 · 最后活跃 {ago(p.lastActive)}
            {daysSince(p.lastActive) > 30 ? `（${daysSince(p.lastActive)} 天前）` : ""}
          </div>
        </div>
        <div style={{ textAlign: "right", flex: "none", fontSize: 12 }}>
          <div className="num" style={{ fontWeight: 600, fontSize: 13 }}>
            {bytes(p.totalBytes)}
          </div>
          <div className="muted num">
            对话 {bytes(p.transcriptsBytes)}
            {p.folderBytes !== null ? ` · 文件夹 ${bytes(p.folderBytes)}` : ""}
          </div>
        </div>
        {filter === "all" ? (
          safeBytes > 0 && <span className="badge good">可放心删 {bytes(safeBytes)}</span>
        ) : (
          <span className={"badge " + (filter === "safe" ? "good" : filter === "review" ? "warning" : "accent")}>
            {safetyLabel(filter)} {bytes(matchedBytes)}
          </span>
        )}
      </div>
      {open && (
        <div>
          {artifacts.length > 0 && (
            <>
              <div className="group-head">
                构建产物与 worktree
                <button className="btn ghost sm" style={{ marginLeft: "auto" }} onClick={() => rp.onToggleMany(leaves(artifacts), true)}>
                  全选可删的
                </button>
              </div>
              {artifacts.map((it) =>
                it.children.length ? (
                  <TreeRow key={it.id} it={it} depth={1} max={it.sizeBytes} {...rp} />
                ) : (
                  <LeafRow key={it.id} it={it} {...rp} />
                ),
              )}
            </>
          )}
          {transcripts.length > 0 && (
            <div className="group-head">
              对话记录 · {transcripts.length}
              {cleanable.length > 0 && transcripts.some((t) => bucket(t) === "safe") && (
                <button className="btn ghost sm" style={{ marginLeft: "auto" }} onClick={() => rp.onToggleMany(transcripts.filter((t) => bucket(t) === "safe"), true)}>
                  勾选可放心删的
                </button>
              )}
            </div>
          )}
          {[...transcripts]
            .sort((a, b) => b.modified.localeCompare(a.modified))
            .map((it) => (
              <LeafRow key={it.id} it={it} {...rp} />
            ))}
        </div>
      )}
    </div>
  );
}

function LeafRow({ it, ...p }: RowProps & { it: DiskItem }) {
  const v = p.verdicts.get(it.id);
  return (
    <div className="row" style={{ paddingLeft: 38 }}>
      <Check it={it} selected={p.selected} onToggle={p.onToggle} />
      <div className="row-main">
        <div className="row-title">
          <span className="truncate">{it.title || it.label}</span>
          <SafetyBadge s={it.safety} />
          {it.inUse && <span className="badge accent">正在使用</span>}
          <Verdict v={v} />
        </div>
        <div className="row-sub truncate" title={v?.reason ?? it.reason}>
          {v ? `AI：${v.reason}` : it.reason}
        </div>
      </div>
      <div style={{ textAlign: "right", flex: "none", width: 96 }}>
        <div className="num" style={{ fontWeight: 500 }}>
          {bytes(it.sizeBytes)}
        </div>
        <div className="muted" style={{ fontSize: 11 }}>
          {ago(it.modified)}
        </div>
      </div>
      <button className="icon-btn" title={`在访达中显示 ${tildify(it.path)}`} onClick={() => api.revealPath(it.path)}>
        <FolderOpen size={14} />
      </button>
    </div>
  );
}
