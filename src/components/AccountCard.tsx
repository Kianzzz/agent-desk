import { Plug } from "lucide-react";
import { useState } from "react";
import { api } from "../api";
import { ago, money, TOOL_LABEL, tokens, toolVar } from "../lib/format";
import type { Account } from "../types";
import type { Totals } from "../lib/usage";
import { totalTokens } from "../lib/usage";
import { QuotaRow } from "./Quota";
import { Spinner } from "./ui";

/** 一个 AI 的账号卡片：等级、5 小时和每周额度、今天和近 30 天的 token。 */
export function AccountCard({ a, today, month, onChanged }: { a: Account; today?: Totals; month?: Totals; onChanged?: () => void }) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const observed = a.windows.map((w) => w.observedAt).sort().pop();
  const canConnect = a.tool === "claude" && a.windows.length === 0 && a.hint?.includes("还没接入");

  return (
    <div className="card card-pad acct">
      <div className="acct-head">
        <div style={{ minWidth: 0 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <span className="dot" style={{ background: toolVar(a.tool) }} />
            <span className="acct-name">{TOOL_LABEL[a.tool]}</span>
            {a.plan && <span className="badge accent">{a.plan}</span>}
          </div>
          <div className="label" style={{ marginTop: 3, paddingLeft: 15 }}>
            {a.loginKind}
          </div>
        </div>
        <div className="acct-tokens">
          <div className="mid-num">{tokens(today ? totalTokens(today.tokens) : 0)}</div>
          <div className="cost-note" style={{ whiteSpace: "nowrap" }}>
            今天 ≈ {money(today?.cost ?? 0)}
          </div>
        </div>
      </div>

      {a.windows.length > 0 ? (
        <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
          {a.windows.map((w) => (
            <QuotaRow key={w.kind + w.label} w={w} segments={20} />
          ))}
        </div>
      ) : (
        <div className="label" style={{ lineHeight: 1.6 }}>
          {a.hint ?? "没有额度数据"}
          {canConnect && (
            <div style={{ marginTop: 8 }}>
              <button
                className="btn sm"
                disabled={busy}
                onClick={async () => {
                  setBusy(true);
                  setErr(null);
                  try {
                    await api.setClaudeBridge(true);
                    onChanged?.();
                  } catch (e) {
                    setErr(String(e));
                  } finally {
                    setBusy(false);
                  }
                }}
              >
                {busy ? <Spinner size={11} /> : <Plug size={12} />} 接入 Claude Code 状态栏
              </button>
              {err && <div style={{ color: "var(--critical-ink)", marginTop: 6 }}>{err}</div>}
            </div>
          )}
        </div>
      )}

      <div className="acct-foot">
        <span>
          近 30 天 <span className="num">{tokens(month ? totalTokens(month.tokens) : 0)}</span> · ≈ {money(month?.cost ?? 0)}
        </span>
        {a.quotaSource && observed && (
          <span style={{ marginLeft: "auto" }} title={a.quotaSource}>
            额度记录于 {ago(observed)}
            {a.windows.some((w) => w.estimated) ? "，之后按本地用量估算" : ""}
          </span>
        )}
      </div>
    </div>
  );
}
