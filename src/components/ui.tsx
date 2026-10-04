import { AlertCircle, AlertTriangle, CheckCircle2, Info, Loader2, ShieldAlert } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import type { Safety, Severity } from "../types";

export function Toggle({ on, onChange, disabled, title }: { on: boolean; onChange: (v: boolean) => void; disabled?: boolean; title?: string }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      title={title}
      className={"toggle" + (on ? " on" : "")}
      disabled={disabled}
      onClick={() => onChange(!on)}
    />
  );
}

export function Segmented<T extends string>({ value, options, onChange }: { value: T; options: { value: T; label: string }[]; onChange: (v: T) => void }) {
  return (
    <div className="segmented" role="tablist">
      {options.map((o) => (
        <button key={o.value} className={o.value === value ? "on" : ""} onClick={() => onChange(o.value)} role="tab" aria-selected={o.value === value}>
          {o.label}
        </button>
      ))}
    </div>
  );
}

export function Spinner({ size = 14 }: { size?: number }) {
  return <Loader2 size={size} className="spin" />;
}

export function Empty({ title, children, icon }: { title: string; children?: ReactNode; icon?: ReactNode }) {
  return (
    <div className="empty">
      {icon && <div style={{ marginBottom: 8 }}>{icon}</div>}
      <div className="title">{title}</div>
      {children && <div>{children}</div>}
    </div>
  );
}

export function Loading({ text = "正在读取…" }: { text?: string }) {
  return (
    <div className="empty">
      <Spinner size={18} />
      <div style={{ marginTop: 8 }}>{text}</div>
    </div>
  );
}

const SEVERITY: Record<Severity, { label: string; cls: string; Icon: typeof AlertCircle }> = {
  high: { label: "高风险", cls: "critical", Icon: ShieldAlert },
  medium: { label: "中风险", cls: "serious", Icon: AlertTriangle },
  low: { label: "低风险", cls: "warning", Icon: AlertCircle },
  info: { label: "提示", cls: "", Icon: Info },
};

export function severityLabel(s: Severity) {
  return SEVERITY[s].label;
}

export function SeverityBadge({ s }: { s: Severity }) {
  const { label, cls, Icon } = SEVERITY[s];
  return (
    <span className={"badge " + cls}>
      <Icon size={12} />
      {label}
    </span>
  );
}

const SAFETY: Record<Safety, { label: string; cls: string; Icon: typeof AlertCircle }> = {
  safe: { label: "可放心删", cls: "good", Icon: CheckCircle2 },
  review: { label: "需确认", cls: "warning", Icon: AlertCircle },
  keep: { label: "建议保留", cls: "accent", Icon: Info },
  protected: { label: "受保护", cls: "", Icon: ShieldAlert },
};

export function safetyLabel(s: Safety) {
  return SAFETY[s].label;
}

export function SafetyBadge({ s }: { s: Safety }) {
  const { label, cls, Icon } = SAFETY[s];
  return (
    <span className={"badge " + cls}>
      <Icon size={12} />
      {label}
    </span>
  );
}

export function Meter({ value, warnAt = 70, critAt = 90 }: { value: number; warnAt?: number; critAt?: number }) {
  const cls = value >= critAt ? " critical" : value >= warnAt ? " warning" : "";
  return (
    <div className={"meter" + cls} role="meter" aria-valuenow={value} aria-valuemin={0} aria-valuemax={100}>
      <i style={{ width: `${Math.min(100, Math.max(0, value))}%` }} />
    </div>
  );
}

export function Toast({ text, onDone }: { text: string | null; onDone: () => void }) {
  useEffect(() => {
    if (!text) return;
    const t = setTimeout(onDone, 3600);
    return () => clearTimeout(t);
  }, [text, onDone]);
  if (!text) return null;
  return <div className="toast">{text}</div>;
}

export function useToast() {
  const [text, setText] = useState<string | null>(null);
  return { text, show: setText, clear: () => setText(null) };
}

export function ErrorBox({ error }: { error: string | null | undefined }) {
  if (!error) return null;
  return <div className="error-box">{error}</div>;
}

export function Confirm({
  title,
  children,
  confirmText,
  danger,
  busy,
  onConfirm,
  onCancel,
}: {
  title: string;
  children: ReactNode;
  confirmText: string;
  danger?: boolean;
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="scrim" onClick={onCancel}>
      <div className="dialog" onClick={(e) => e.stopPropagation()} role="dialog" aria-modal>
        <div className="dialog-body">
          <h3 style={{ margin: "0 0 10px", fontSize: 15 }}>{title}</h3>
          {children}
        </div>
        <div className="dialog-foot">
          <button className="btn" onClick={onCancel} disabled={busy}>
            取消
          </button>
          <button className={"btn " + (danger ? "danger" : "primary")} onClick={onConfirm} disabled={busy}>
            {busy && <Spinner size={12} />}
            {confirmText}
          </button>
        </div>
      </div>
    </div>
  );
}
