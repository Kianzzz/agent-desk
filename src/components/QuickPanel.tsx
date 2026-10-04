import { ArrowUpRight, RefreshCw } from "lucide-react";
import { useState } from "react";
import { api } from "../api";
import { addDays, ago, localDate, money, TOOL_LABEL, tokens, toolVar } from "../lib/format";
import { useAccounts, useTick, useUsage } from "../lib/hooks";
import { byTool, rowsInRange, todayAndYesterday, totalTokens } from "../lib/usage";
import { QuotaRow } from "./Quota";
import { Spinner } from "./ui";

/** 边缘细条展开后、菜单栏卡片里显示的内容：今天的 token、各 AI 的等级和额度。 */
export function QuickPanel({ onOpen }: { onOpen: (page?: string) => void }) {
  useTick(30000);
  const [usage, reloadUsage] = useUsage();
  const accounts = useAccounts();
  const [refreshing, setRefreshing] = useState(false);
  const snap = usage?.snapshot ?? null;

  const refresh = async () => {
    setRefreshing(true);
    try {
      await api.refreshUsage();
      await reloadUsage();
    } finally {
      setRefreshing(false);
    }
  };

  if (!snap) {
    return (
      <div className="empty" style={{ margin: "auto" }}>
        {usage?.error ?? (
          <>
            <Spinner size={16} />
            <div style={{ marginTop: 8 }}>第一次读取日志，稍等几秒…</div>
          </>
        )}
      </div>
    );
  }

  const { today, todayRows } = todayAndYesterday(snap);
  const todayByTool = byTool(todayRows);
  const recent = new Set(rowsInRange(snap, addDays(localDate(), -29)).map((r) => r.tool));
  const shown = (accounts ?? []).filter((a) => a.loggedIn);

  return (
    <>
      <div style={{ display: "flex", alignItems: "flex-start" }}>
        <div>
          <div className="q-total">{tokens(totalTokens(today.tokens))}</div>
          <div className="label" style={{ marginTop: 2 }}>
            今天 tokens · ≈ {money(today.cost)}
          </div>
        </div>
        <button className="icon-btn" style={{ marginLeft: "auto" }} title="立即刷新" onClick={refresh}>
          <RefreshCw size={13} className={refreshing ? "spin" : ""} />
        </button>
      </div>

      {shown.map((a) => {
        const t = todayByTool.get(a.tool);
        const name = TOOL_LABEL[a.tool];
        if (!t && a.windows.length === 0 && !recent.has(a.tool)) {
          return (
            <div className="q-sec label" key={a.tool}>
              {name} · 30 天没用过
            </div>
          );
        }
        return (
          <div className="q-sec" key={a.tool}>
            <div className="q-head">
              <span className="dot" style={{ background: toolVar(a.tool), alignSelf: "center" }} />
              <span>{name}</span>
              <span className="plan">{a.plan ?? a.loginKind}</span>
              <span className="tok">{tokens(t ? totalTokens(t.tokens) : 0)}</span>
            </div>
            {a.windows.map((w) => (
              <QuotaRow key={w.kind + w.label} w={w} />
            ))}
            {a.windows.length === 0 && a.hint && (
              <div className="label" style={{ marginTop: 6, lineHeight: 1.5 }}>
                {a.hint}
              </div>
            )}
            {t && t.cost > 0 && (
              <div className="cost-note" style={{ marginTop: 6 }}>
                ≈ {money(t.cost)}
              </div>
            )}
          </div>
        );
      })}

      <div className="q-foot">
        <span>更新于 {ago(snap.generatedAt)}</span>
        <button className="btn sm" style={{ marginLeft: "auto" }} onClick={() => onOpen()}>
          打开面板 <ArrowUpRight size={12} />
        </button>
      </div>
    </>
  );
}
