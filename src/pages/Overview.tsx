import { ArrowRight, Blocks, HardDrive, RefreshCw, ShieldAlert } from "lucide-react";
import { useEffect, useState } from "react";
import { api } from "../api";
import type { Page } from "../app/MainApp";
import { AccountCard } from "../components/AccountCard";
import { StackedBars, ToolLegend } from "../components/StackedBars";
import { ErrorBox, Loading } from "../components/ui";
import { sumSafe } from "../lib/disk";
import { addDays, ago, bytes, localDate, money, TOOL_LABEL, tokens, toolVar } from "../lib/format";
import { useAccounts, useDisk, useSecurity, useSettings, useTick, useUsage } from "../lib/hooks";
import { byModel, byTool, dailySeries, rowsInRange, sumRows, todayAndYesterday, totalTokens } from "../lib/usage";
import type { Inventory, Tool } from "../types";

export function Overview({ go }: { go: (p: Page) => void }) {
  useTick();
  const [usage, reload] = useUsage();
  const accounts = useAccounts();
  const [settings] = useSettings();
  const [security] = useSecurity();
  const [disk] = useDisk();
  const [inv, setInv] = useState<Inventory | null>(null);
  const [refreshing, setRefreshing] = useState(false);

  useEffect(() => {
    api.getInventory().then(setInv).catch(() => {});
  }, []);

  const snap = usage?.snapshot;
  const head = (
    <div className="page-head">
      <div>
        <h1>概览</h1>
        <div className="sub">token 来自各 AI 的本机日志；费用按 API 公开价格折算，只作参考，订阅不会这样扣费。</div>
      </div>
      <div className="actions">
        {snap && <span className="label">更新于 {ago(snap.generatedAt)}</span>}
        <button
          className="btn"
          disabled={refreshing}
          onClick={async () => {
            setRefreshing(true);
            await api.refreshUsage().catch(() => {});
            await reload();
            setRefreshing(false);
          }}
        >
          <RefreshCw size={12} className={refreshing ? "spin" : ""} /> 刷新
        </button>
      </div>
    </div>
  );

  if (!snap) {
    return (
      <div className="page">
        {head}
        {usage?.error ? <ErrorBox error={usage.error} /> : <Loading text="第一次读取各个 AI 的日志，大约需要十几秒…" />}
      </div>
    );
  }

  const { today, yesterday, todayRows } = todayAndYesterday(snap);
  const todayTokens = totalTokens(today.tokens);
  const yTokens = totalTokens(yesterday.tokens);
  const deltaPct = yTokens > 0 ? ((todayTokens - yTokens) / yTokens) * 100 : null;
  const from30 = addDays(localDate(), -29);
  const rows30 = rowsInRange(snap, from30);
  const month = sumRows(rows30);
  const month7 = sumRows(rowsInRange(snap, addDays(localDate(), -6)));
  const series = dailySeries(snap, 30);
  const todayTools = byTool(todayRows);
  const tools30 = byTool(rows30);
  const toolOrder = (["claude", "codex", "gemini"] as Tool[]).filter((t) => tools30.has(t));
  const models = byModel(todayRows).sort((a, b) => totalTokens(b.tokens) - totalTokens(a.tokens));
  const loggedIn = (accounts ?? []).filter((a) => a.loggedIn);
  // 30 天内用过、或有额度数据的给完整卡片；其余收成一行
  const shown = loggedIn.filter((a) => tools30.has(a.tool) || a.windows.length > 0);
  const idle = loggedIn.filter((a) => !shown.includes(a));

  const ignored = new Set(settings?.ignoredFindings ?? []);
  const risky = security?.findings.filter((f) => (f.severity === "high" || f.severity === "medium") && !ignored.has(f.id)).length ?? 0;
  const conflicts = inv?.skillGroups.filter((g) => !g.identical).length ?? 0;
  const safe = disk ? sumSafe(disk.locations) : 0;

  return (
    <div className="page">
      {head}

      <div className="card card-pad" style={{ display: "flex", alignItems: "flex-end", gap: 32, flexWrap: "wrap" }}>
        <div>
          <div className="label">今天</div>
          <div className="big-num" style={{ marginTop: 4 }}>
            {tokens(todayTokens)}
            <span style={{ fontSize: 14, color: "var(--text-muted)", marginLeft: 8, letterSpacing: 0 }}>tokens</span>
          </div>
          <div className="cost-note" style={{ marginTop: 6 }}>
            ≈ {money(today.cost)} · {today.requests.toLocaleString()} 次请求
            {deltaPct !== null && (
              <>
                {" · 比昨天 "}
                <span className={deltaPct > 0 ? "delta-up" : "delta-down"}>
                  {deltaPct >= 0 ? "+" : "−"}
                  {Math.abs(deltaPct).toFixed(0)}%
                </span>
              </>
            )}
          </div>
        </div>
        <div style={{ display: "flex", gap: 36, marginLeft: "auto" }}>
          <Mini label="近 7 天" tok={totalTokens(month7.tokens)} cost={month7.cost} />
          <Mini label="近 30 天" tok={totalTokens(month.tokens)} cost={month.cost} />
          <Mini label="30 天日均" tok={totalTokens(month.tokens) / 30} cost={month.cost / 30} />
        </div>
      </div>

      <div className="section-title" style={{ marginTop: 22 }}>
        账号与额度
      </div>
      {accounts === null ? (
        <Loading />
      ) : loggedIn.length === 0 ? (
        <div className="card empty">没有检测到已登录的 AI 账号</div>
      ) : (
        <>
          {shown.length > 0 && (
            <div className="grid" style={{ gridTemplateColumns: `repeat(${Math.min(shown.length, 3)}, minmax(0, 1fr))` }}>
              {shown.map((a) => (
                <AccountCard key={a.tool} a={a} today={todayTools.get(a.tool)} month={tools30.get(a.tool)} onChanged={reload} />
              ))}
            </div>
          )}
          {idle.length > 0 && (
            <div className="label" style={{ display: "flex", gap: 18, marginTop: 10 }}>
              {idle.map((a) => (
                <span key={a.tool} style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                  <span className="dot" style={{ background: toolVar(a.tool), opacity: 0.6 }} />
                  {TOOL_LABEL[a.tool]} · {a.plan ?? a.loginKind} · 30 天没用过
                </span>
              ))}
            </div>
          )}
        </>
      )}

      <div className="card card-pad section">
        <div className="card-title">
          最近 30 天 tokens
          <span className="right">
            <ToolLegend tools={toolOrder} />
          </span>
        </div>
        <StackedBars points={series} tools={toolOrder} height={200} metric="tokens" />
      </div>

      <div className="grid section" style={{ gridTemplateColumns: "minmax(0, 3fr) minmax(0, 2fr)" }}>
        <div className="card">
          <div className="card-title card-pad" style={{ margin: 0, paddingBottom: 6 }}>
            今天按模型
            <button className="btn ghost sm" style={{ marginLeft: "auto" }} onClick={() => go("usage")}>
              详细 <ArrowRight size={12} />
            </button>
          </div>
          {models.length === 0 ? (
            <div className="empty">今天还没有用量</div>
          ) : (
            <table className="table">
              <thead>
                <tr>
                  <th style={{ paddingLeft: 18 }}>模型</th>
                  <th className="r">请求</th>
                  <th className="r">tokens</th>
                  <th className="r" style={{ paddingRight: 18 }}>
                    ≈ 费用
                  </th>
                </tr>
              </thead>
              <tbody>
                {models.slice(0, 8).map((m) => (
                  <tr key={m.tool + m.model}>
                    <td style={{ paddingLeft: 18 }}>
                      <span style={{ display: "inline-flex", alignItems: "center", gap: 8 }}>
                        <span className="dot" style={{ background: toolVar(m.tool) }} />
                        <span className="mono">{m.model}</span>
                      </span>
                    </td>
                    <td className="r">{m.requests.toLocaleString()}</td>
                    <td className="r">{tokens(totalTokens(m.tokens))}</td>
                    <td className="r dim" style={{ paddingRight: 18 }}>
                      {m.unpriced === m.requests ? "—" : money(m.cost)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>

        <div className="card">
          <div className="card-title card-pad" style={{ margin: 0, paddingBottom: 6 }}>
            待处理
          </div>
          <div className="list">
            <Todo icon={<ShieldAlert size={14} />} label="中高风险" value={security ? `${risky} 项` : "扫描中…"} hot={risky > 0} onClick={() => go("security")} />
            <Todo icon={<HardDrive size={14} />} label="可放心删" value={disk ? bytes(safe) : "统计中…"} onClick={() => go("cleanup")} />
            <Todo icon={<Blocks size={14} />} label="内容不一致的技能" value={inv ? `${conflicts} 组` : "读取中…"} hot={conflicts > 0} onClick={() => go("extensions")} />
          </div>
        </div>
      </div>
    </div>
  );
}

function Mini({ label, tok, cost }: { label: string; tok: number; cost: number }) {
  return (
    <div>
      <div className="label">{label}</div>
      <div className="mid-num" style={{ marginTop: 2 }}>
        {tokens(tok)}
      </div>
      <div className="cost-note">≈ {money(cost)}</div>
    </div>
  );
}

function Todo({ icon, label, value, hot, onClick }: { icon: React.ReactNode; label: string; value: string; hot?: boolean; onClick: () => void }) {
  return (
    <button className="row" onClick={onClick} style={{ border: 0, borderBottom: "1px solid var(--grid)", background: "transparent", width: "100%", textAlign: "left", cursor: "pointer" }}>
      <span className="muted">{icon}</span>
      <span className="row-main secondary">{label}</span>
      <span className="num" style={{ color: hot ? "var(--warning-ink)" : "var(--text-primary)" }}>
        {value}
      </span>
      <ArrowRight size={12} className="muted" />
    </button>
  );
}
