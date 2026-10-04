import { CloudDownload, Info } from "lucide-react";
import { useMemo, useState } from "react";
import { api } from "../api";
import { StackedBars, ToolLegend } from "../components/StackedBars";
import { ErrorBox, Loading, Segmented, Spinner, Toast, useToast } from "../components/ui";
import { addDays, ago, basename, int, localDate, money, TOOL_LABEL, tildify, tokens, toolVar } from "../lib/format";
import { useAction, useUsage } from "../lib/hooks";
import { byModel, dailySeries, firstDate, rowsInRange, sumRows, totalTokens } from "../lib/usage";
import type { Tool } from "../types";

type Range = "1" | "7" | "30" | "90" | "all";

export function Usage() {
  const [usage, reload] = useUsage();
  const [range, setRange] = useState<Range>("30");
  const [toolFilter, setToolFilter] = useState<Set<Tool>>(new Set());
  const [table, setTable] = useState<"model" | "day">("model");
  const toast = useToast();
  const pricing = useAction(api.updatePricing);

  const snap = usage?.snapshot ?? null;
  const allTools = useMemo(
    () => (["claude", "codex", "gemini"] as Tool[]).filter((t) => snap?.days.some((d) => d.tool === t)),
    [snap],
  );

  if (!snap) {
    return (
      <div className="page">
        <div className="page-head">
          <h1>用量</h1>
        </div>
        {usage?.error ? <ErrorBox error={usage.error} /> : <Loading />}
      </div>
    );
  }

  const today = localDate();
  const first = firstDate(snap) ?? today;
  const days = range === "all" ? Math.max(1, Math.round((Date.parse(today) - Date.parse(first)) / 86400000) + 1) : Number(range);
  const from = range === "all" ? null : addDays(today, -(days - 1));
  const active = toolFilter.size ? toolFilter : new Set(allTools);
  const rows = rowsInRange(snap, from).filter((r) => active.has(r.tool));
  const totals = sumRows(rows);
  const models = byModel(rows).sort((a, b) => totalTokens(b.tokens) - totalTokens(a.tokens));
  const tools = allTools.filter((t) => active.has(t));
  const series = dailySeries(snap, Math.min(days, 400), active);
  const projects = snap.projects
    .filter((p) => active.has(p.tool) && (from === null || p.lastActive.slice(0, 10) >= from))
    .sort((a, b) => totalTokens(b.tokens) - totalTokens(a.tokens))
    .slice(0, 12);
  const sessions = snap.sessions.filter((s) => active.has(s.tool) && (from === null || s.lastActive.slice(0, 10) >= from)).slice(0, 15);
  const dayRows = [...new Set(rows.map((r) => r.date))].sort().reverse();
  const allTok = totalTokens(totals.tokens);
  const hit = totals.tokens.cacheRead + totals.tokens.input + totals.tokens.cacheWrite;

  const toggleTool = (t: Tool) => {
    const next = new Set(toolFilter);
    if (next.has(t)) next.delete(t);
    else next.add(t);
    setToolFilter(next.size === allTools.length ? new Set() : next);
  };

  return (
    <div className="page">
      <div className="page-head">
        <div>
          <h1>用量</h1>
          <div className="sub">缓存读写单独计数；费用按 API 公开价格折算，仅供参考。</div>
        </div>
        <div className="actions">
          <div className="chips">
            {allTools.map((t) => (
              <button key={t} className={"chip" + (toolFilter.has(t) ? " on" : "")} onClick={() => toggleTool(t)}>
                <span className="dot" style={{ background: toolVar(t) }} />
                {TOOL_LABEL[t]}
              </button>
            ))}
          </div>
          <Segmented<Range>
            value={range}
            onChange={setRange}
            options={[
              { value: "1", label: "今天" },
              { value: "7", label: "7 天" },
              { value: "30", label: "30 天" },
              { value: "90", label: "90 天" },
              { value: "all", label: "全部" },
            ]}
          />
        </div>
      </div>

      <div className="grid cols-4">
        <Stat label="tokens 合计" value={tokens(allTok)} note={`≈ ${money(totals.cost)} · 日均 ${tokens(allTok / days)}`} />
        <Stat label="输入 / 输出" value={`${tokens(totals.tokens.input)} / ${tokens(totals.tokens.output)}`} note={`其中推理 ${tokens(totals.tokens.reasoning)}`} />
        <Stat label="缓存读 / 写" value={`${tokens(totals.tokens.cacheRead)} / ${tokens(totals.tokens.cacheWrite)}`} note={`缓存命中 ${hit ? Math.round((totals.tokens.cacheRead / hit) * 100) : 0}%`} />
        <Stat label="请求" value={int(totals.requests)} note={totals.unpriced ? `${int(totals.unpriced)} 次没有价格` : "全部有价格"} />
      </div>

      {days > 1 && (
        <div className="card card-pad section">
          <div className="card-title">
            每日 tokens
            <span className="right">
              <ToolLegend tools={tools} />
            </span>
          </div>
          <StackedBars points={series} tools={tools} height={220} metric="tokens" />
        </div>
      )}

      <div className="section card">
        <div className="card-pad" style={{ display: "flex", alignItems: "center", paddingBottom: 8 }}>
          <Segmented
            value={table}
            onChange={setTable}
            options={[
              { value: "model", label: "按模型" },
              { value: "day", label: "按天" },
            ]}
          />
        </div>
        {table === "model" ? (
          <table className="table">
            <thead>
              <tr>
                <th style={{ paddingLeft: 18 }}>模型</th>
                <th className="r">请求</th>
                <th className="r">输入</th>
                <th className="r">输出</th>
                <th className="r">缓存读</th>
                <th className="r">缓存写</th>
                <th className="r">合计</th>
                <th className="r" style={{ paddingRight: 18 }}>
                  ≈ 费用
                </th>
              </tr>
            </thead>
            <tbody>
              {models.map((m) => (
                <tr key={m.tool + m.model}>
                  <td style={{ paddingLeft: 18 }}>
                    <span style={{ display: "inline-flex", alignItems: "center", gap: 8 }}>
                      <span className="dot" style={{ background: toolVar(m.tool) }} />
                      <span className="mono">{m.model}</span>
                    </span>
                  </td>
                  <td className="r dim">{int(m.requests)}</td>
                  <td className="r">{tokens(m.tokens.input)}</td>
                  <td className="r">{tokens(m.tokens.output)}</td>
                  <td className="r">{tokens(m.tokens.cacheRead)}</td>
                  <td className="r">{tokens(m.tokens.cacheWrite)}</td>
                  <td className="r" style={{ color: "var(--text-primary)" }}>
                    {tokens(totalTokens(m.tokens))}
                  </td>
                  <td className="r dim" style={{ paddingRight: 18 }}>
                    {m.unpriced === m.requests ? "—" : money(m.cost)}
                  </td>
                </tr>
              ))}
            </tbody>
            <tfoot>
              <tr>
                <td style={{ paddingLeft: 18 }}>合计</td>
                <td className="r">{int(totals.requests)}</td>
                <td className="r">{tokens(totals.tokens.input)}</td>
                <td className="r">{tokens(totals.tokens.output)}</td>
                <td className="r">{tokens(totals.tokens.cacheRead)}</td>
                <td className="r">{tokens(totals.tokens.cacheWrite)}</td>
                <td className="r">{tokens(allTok)}</td>
                <td className="r dim" style={{ paddingRight: 18 }}>
                  {money(totals.cost)}
                </td>
              </tr>
            </tfoot>
          </table>
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th style={{ paddingLeft: 18 }}>日期</th>
                {tools.map((t) => (
                  <th key={t} className="r">
                    {TOOL_LABEL[t]}
                  </th>
                ))}
                <th className="r">合计</th>
                <th className="r" style={{ paddingRight: 18 }}>
                  ≈ 费用
                </th>
              </tr>
            </thead>
            <tbody>
              {dayRows.map((d) => {
                const r = rows.filter((x) => x.date === d);
                const s = sumRows(r);
                return (
                  <tr key={d}>
                    <td style={{ paddingLeft: 18 }} className="num">
                      {d}
                    </td>
                    {tools.map((t) => (
                      <td key={t} className="r">
                        {tokens(totalTokens(sumRows(r.filter((x) => x.tool === t)).tokens))}
                      </td>
                    ))}
                    <td className="r" style={{ color: "var(--text-primary)" }}>
                      {tokens(totalTokens(s.tokens))}
                    </td>
                    <td className="r dim" style={{ paddingRight: 18 }}>
                      {money(s.cost)}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </div>

      <div className="grid cols-2 section">
        <div className="card">
          <div className="card-title card-pad" style={{ margin: 0, paddingBottom: 4 }}>
            用得最多的项目
          </div>
          {projects.length === 0 ? (
            <div className="empty">这个时间段没有项目</div>
          ) : (
            <div className="list">
              {projects.map((p) => (
                <div className="row" key={p.tool + p.project}>
                  <span className="dot" style={{ background: toolVar(p.tool) }} />
                  <div className="row-main">
                    <div className="row-title truncate">{basename(p.project)}</div>
                    <div className="row-sub truncate">
                      {tildify(p.project)} · {p.sessions} 个对话 · {ago(p.lastActive)}
                    </div>
                  </div>
                  <div style={{ textAlign: "right" }}>
                    <div className="num">{tokens(totalTokens(p.tokens))}</div>
                    <div className="cost-note">≈ {money(p.costUsd)}</div>
                  </div>
                </div>
              ))}
            </div>
          )}
          <div className="label card-pad" style={{ paddingTop: 8 }}>
            项目是累计值，按最后活跃时间筛选。
          </div>
        </div>
        <div className="card">
          <div className="card-title card-pad" style={{ margin: 0, paddingBottom: 4 }}>
            最近的对话
          </div>
          {sessions.length === 0 ? (
            <div className="empty">这个时间段没有对话</div>
          ) : (
            <div className="list">
              {sessions.map((s) => (
                <div className="row" key={s.tool + s.sessionId}>
                  <span className="dot" style={{ background: toolVar(s.tool) }} />
                  <div className="row-main">
                    <div className="row-title truncate">{s.title || <span className="muted">（没有标题）</span>}</div>
                    <div className="row-sub truncate">
                      {basename(s.project)} · {s.models.join("、")} · {ago(s.lastActive)}
                    </div>
                  </div>
                  <div style={{ textAlign: "right" }}>
                    <div className="num">{tokens(totalTokens(s.tokens))}</div>
                    <div className="cost-note">≈ {money(s.costUsd)}</div>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>

      <div className="card card-pad section">
        <div className="card-title">
          数据来源
          <span className="right">
            价格表更新于 {snap.pricingUpdatedAt ? ago(snap.pricingUpdatedAt) : "—"}
            <button
              className="btn sm"
              style={{ marginLeft: 10 }}
              disabled={pricing.busy}
              onClick={async () => {
                const n = await pricing.run();
                if (n !== undefined) {
                  toast.show(`价格表已更新，共 ${n} 个模型`);
                  reload();
                }
              }}
            >
              {pricing.busy ? <Spinner size={12} /> : <CloudDownload size={12} />} 更新价格表
            </button>
          </span>
        </div>
        <ErrorBox error={pricing.error} />
        <table className="table">
          <thead>
            <tr>
              <th>工具</th>
              <th>日志位置</th>
              <th className="r">文件</th>
              <th className="r">记录</th>
              <th className="r">已删文件的记录</th>
              <th>最近一条</th>
            </tr>
          </thead>
          <tbody>
            {snap.sources.map((s) => (
              <tr key={s.tool + s.root}>
                <td>{TOOL_LABEL[s.tool]}</td>
                <td className="mono truncate" style={{ maxWidth: 320 }}>
                  {tildify(s.root)}
                </td>
                <td className="r">{int(s.files)}</td>
                <td className="r">{int(s.records)}</td>
                <td className="r">{int(s.archivedRecords)}</td>
                <td>
                  {ago(s.lastRecordAt)}
                  {s.errors.length > 0 && (
                    <span className="badge warning" style={{ marginLeft: 6 }} title={s.errors.join("\n")}>
                      {s.errors.length} 个问题
                    </span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {snap.unpricedModels.length > 0 && (
          <div className="note" style={{ marginTop: 10 }}>
            <Info size={14} />
            <span>
              这些模型在价格表里找不到，token 照常统计，费用不计入：<span className="mono">{snap.unpricedModels.join("、")}</span>
            </span>
          </div>
        )}
        <div className="label" style={{ marginTop: 10 }}>
          对话记录被删除后，Agent Desk 缓存里的用量仍会保留。
        </div>
      </div>
      <Toast text={toast.text} onDone={toast.clear} />
    </div>
  );
}

function Stat({ label, value, note }: { label: string; value: string; note?: string }) {
  return (
    <div className="card card-pad">
      <div className="label">{label}</div>
      <div className="mid-num" style={{ marginTop: 4 }}>
        {value}
      </div>
      {note && (
        <div className="cost-note" style={{ marginTop: 2 }}>
          {note}
        </div>
      )}
    </div>
  );
}
