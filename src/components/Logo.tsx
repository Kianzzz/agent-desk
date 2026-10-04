/** Agent Desk 的标志：两圈额度环（外圈 5 小时、内圈本周）加圆心。跟着主题变色。 */
export function Logo({ size = 18 }: { size?: number }) {
  // 外圈周长 2π·27 ≈ 169.6，填 72%；内圈 2π·16.5 ≈ 103.7，填 54%
  return (
    <svg width={size} height={size} viewBox="0 0 100 100" aria-hidden="true" style={{ flex: "none" }}>
      <circle cx="50" cy="50" r="40" fill="none" stroke="var(--seg-off)" strokeWidth="13" />
      <circle cx="50" cy="50" r="40" fill="none" stroke="var(--tool-claude)" strokeWidth="13" strokeLinecap="round" strokeDasharray="181 252" transform="rotate(-90 50 50)" />
      <circle cx="50" cy="50" r="22" fill="none" stroke="var(--seg-off)" strokeWidth="13" />
      <circle cx="50" cy="50" r="22" fill="none" stroke="var(--tool-codex)" strokeWidth="13" strokeLinecap="round" strokeDasharray="75 138" transform="rotate(-90 50 50)" />
      <circle cx="50" cy="50" r="7" fill="var(--text-primary)" />
    </svg>
  );
}
