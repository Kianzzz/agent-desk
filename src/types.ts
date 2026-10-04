// 与 crates/*/src/lib.rs 的公共类型一一对应（serde camelCase）。

// ---------- ad-usage ----------
export type Tool = "claude" | "codex" | "gemini";

export interface TokenCounts {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  reasoning: number;
}

export interface DailyModelRow {
  date: string;
  tool: Tool;
  model: string;
  tokens: TokenCounts;
  costUsd: number;
  requests: number;
  unpricedRequests: number;
}

export interface ProjectRow {
  project: string;
  tool: Tool;
  tokens: TokenCounts;
  costUsd: number;
  requests: number;
  sessions: number;
  firstActive: string;
  lastActive: string;
}

export interface SessionRow {
  sessionId: string;
  tool: Tool;
  project: string | null;
  title: string | null;
  models: string[];
  startedAt: string;
  lastActive: string;
  tokens: TokenCounts;
  costUsd: number;
  requests: number;
}

export interface QuotaWindow {
  tool: Tool;
  label: string;
  usedPercent: number;
  windowMinutes: number;
  resetsAt: string | null;
  observedAt: string;
}

export interface ActiveBlock {
  tool: Tool;
  start: string;
  end: string;
  costUsd: number;
  tokens: TokenCounts;
  requests: number;
  burnRateUsdPerHour: number;
}

export interface SourceStatus {
  tool: Tool;
  root: string;
  files: number;
  records: number;
  archivedRecords: number;
  lastRecordAt: string | null;
  errors: string[];
}

export interface UsageSnapshot {
  generatedAt: string;
  timezone: string;
  days: DailyModelRow[];
  projects: ProjectRow[];
  sessions: SessionRow[];
  quotas: QuotaWindow[];
  activeBlocks: ActiveBlock[];
  sources: SourceStatus[];
  pricingUpdatedAt: string | null;
  unpricedModels: string[];
}

export interface UsageState {
  snapshot: UsageSnapshot | null;
  error: string | null;
}

// ---------- ad-quota ----------
export interface QuotaWin {
  kind: "fiveHour" | "weekly" | "other";
  label: string;
  usedPercent: number;
  resetsAt: string | null;
  observedAt: string;
  /** 刷新时间已过，数字是旧的 */
  expired: boolean;
}

export interface Account {
  tool: Tool;
  loggedIn: boolean;
  loginKind: string | null;
  plan: string | null;
  windows: QuotaWin[];
  quotaSource: string | null;
  hint: string | null;
}

export type BridgeStatus = "installed" | "notInstalled" | "broken";

// ---------- ad-inventory ----------
export type Client = "claudeCode" | "claudeDesktop" | "codex" | "gemini" | "cursor";

export interface McpServer {
  id: string;
  client: Client;
  name: string;
  scope: string;
  scopePath: string | null;
  transport: string;
  command: string | null;
  args: string[];
  url: string | null;
  envKeys: string[];
  headerKeys: string[];
  enabled: boolean;
  manageable: boolean;
  configPath: string;
  package: string | null;
}

export interface SkillRoot {
  id: string;
  label: string;
  path: string;
  exists: boolean;
  count: number;
}

export interface SkillEntry {
  id: string;
  name: string;
  description: string;
  rootId: string;
  rootLabel: string;
  path: string;
  realPath: string;
  isSymlink: boolean;
  sizeBytes: number;
  fileCount: number;
  modified: string;
  contentHash: string;
  enabled: boolean;
  manageable: boolean;
  hasScripts: boolean;
  /** 磁盘上的目录名，可能和 frontmatter 里的 name 不同 */
  dirName: string;
  /** 由哪个工具同步进来（目前只有 "ChatCut"），停用后可能被同步回来 */
  syncedBy: string | null;
  /** 停用时条目的存放位置 */
  archivedPath: string | null;
}

export interface SkillGroup {
  name: string;
  entryIds: string[];
  identical: boolean;
}

export interface HookEntry {
  id: string;
  client: Client;
  scope: string;
  scopePath: string | null;
  event: string;
  matcher: string | null;
  command: string;
  timeoutSec: number | null;
  enabled: boolean;
  manageable: boolean;
  configPath: string;
}

export interface PluginEntry {
  id: string;
  name: string;
  marketplace: string | null;
  version: string | null;
  enabled: boolean;
  path: string;
  skills: number;
  mcpServers: number;
  hooks: number;
  commands: number;
  agents: number;
}

export interface Inventory {
  scannedAt: string;
  mcp: McpServer[];
  skillRoots: SkillRoot[];
  skills: SkillEntry[];
  skillGroups: SkillGroup[];
  hooks: HookEntry[];
  plugins: PluginEntry[];
  warnings: string[];
}

// ---------- ad-security ----------
export type Severity = "high" | "medium" | "low" | "info";
export type Category =
  | "hiddenChars"
  | "promptInjection"
  | "dangerousCommand"
  | "plaintextSecret"
  | "broadPermission"
  | "supplyChain"
  | "insecureTransport"
  | "filePermission";

export interface Finding {
  id: string;
  ruleId: string;
  severity: Severity;
  category: Category;
  title: string;
  detail: string;
  path: string;
  line: number | null;
  excerpt: string | null;
  targetKind: string;
  targetName: string | null;
}

export interface ScanReport {
  scannedAt: string;
  filesScanned: number;
  durationMs: number;
  findings: Finding[];
}

// ---------- ad-disk ----------
export type Safety = "safe" | "review" | "keep" | "protected";

export interface DiskItem {
  id: string;
  path: string;
  label: string;
  category: string;
  tool: string;
  project: string | null;
  sizeBytes: number;
  fileCount: number;
  modified: string;
  safety: Safety;
  reason: string;
  inUse: boolean;
  title: string | null;
  children: DiskItem[];
}

export interface ProjectUsage {
  project: string;
  displayName: string;
  exists: boolean;
  lastActive: string;
  sessions: number;
  tools: string[];
  transcriptsBytes: number;
  folderBytes: number | null;
  artifacts: DiskItem[];
  transcripts: DiskItem[];
  totalBytes: number;
}

export interface DiskReport {
  scannedAt: string;
  durationMs: number;
  totalAiBytes: number;
  diskTotalBytes: number;
  diskFreeBytes: number;
  locations: DiskItem[];
  projects: ProjectUsage[];
}

export interface TrashResult {
  path: string;
  ok: boolean;
  freedBytes: number;
  error: string | null;
}

export interface AssessInput {
  id: string;
  path: string;
  label: string;
  category: string;
  project: string | null;
  projectExists: boolean | null;
  sizeBytes: number;
  modified: string;
  title: string | null;
  safety: Safety;
  reason: string;
}

export interface Assessment {
  id: string;
  verdict: "delete" | "keep" | "review";
  reason: string;
}

// ---------- 设置 ----------
export interface Subscription {
  tool: Tool;
  name: string;
  monthlyUsd: number;
}

export type Theme = "dark" | "light" | "system";

export interface Settings {
  theme: Theme;
  edgeEnabled: boolean;
  edgeSide: "right" | "left";
  trayEnabled: boolean;
  trayShowCost: boolean;
  currency: "CNY" | "USD";
  cnyRate: number;
  subscriptions: Subscription[];
  ignoredFindings: string[];
  launchAtLogin: boolean;
  refreshSeconds: number;
}
