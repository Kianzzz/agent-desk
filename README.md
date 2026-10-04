# Agent Desk

macOS 上的本地 AI Agent 管理工具：看各家 AI 每天用了多少 token、订阅额度还剩多少，集中管理 MCP、技能和钩子，做安全检查，清理 AI 工具留下的文件。

所有数据都在本机读取，不经过任何代理或网关。

## 功能

- **用量**：读 Claude Code、Codex、Gemini CLI 留在本机的日志，按天、按模型、按项目统计 token；费用按 [LiteLLM](https://github.com/BerriAI/litellm) 公开价格表折算成美元，只作参考。对话记录删除后，已统计的用量会保留。
- **账号与额度**：自动识别各家的登录状态和账号等级（如 Claude Max 5x、ChatGPT Pro），显示 5 小时和每周额度、什么时候刷新。没登录的工具不显示。
- **MCP · 技能 · 钩子**：汇总 Claude Code、Claude 桌面版、Codex、Gemini、Cursor 的 MCP 服务，各技能目录里的技能（标出重复和内容不一致的），以及钩子和插件。停用可撤销，修改配置前自动备份。
- **安全**：只读检查配置、技能、钩子、项目说明文件和 Shell 配置里的隐藏字符、提示词注入、危险命令、明文密钥、过宽的权限。
- **空间清理**：按位置、按项目统计 AI 产生的文件，分成「可放心删 / 需确认 / 建议保留 / 受保护」；可以让本机的 `claude` 命令再评估一遍。删除只移到废纸篓。
- **快速查看**：鼠标贴到屏幕边缘弹出今天的 token 和各家额度；菜单栏图标旁显示今天的 token。
- 深色、浅色、跟随系统三种外观。

## 支持的工具

用量（token 和折算费用）能读这些工具留在本机的记录：

| 工具 | 本机记录位置 | 说明 |
|---|---|---|
| Claude Code（终端、桌面版的 Code 标签、IDE 插件） | `~/.claude/projects` | 有账号等级；额度需接入状态栏，见下文 |
| Codex（命令行、桌面版） | `~/.codex/sessions` | 有账号等级和额度 |
| Gemini CLI | `~/.gemini/tmp` | 只显示登录方式 |
| Grok Build（xAI 官方）、grok-dev（社区版） | `~/.grok` | 自带费用 |
| OpenCode、Kilo CLI | `~/.local/share/opencode`、`~/.local/share/kilo` | |
| Qwen Code | `~/.qwen/projects` | |
| GitHub Copilot CLI | `~/.copilot` | |
| Cline、Roo Code、Kilo Code（VS Code、Cursor、Windsurf 等编辑器里的扩展） | 编辑器的 `globalStorage`、`~/.cline` | 自带费用 |
| Kimi CLI、Kimi Code | `~/.kimi`、`~/.kimi-code` | |
| Amp | `~/.local/share/amp/threads` | |
| Pi、OpenClaw | `~/.pi`、`~/.openclaw` | 自带费用 |
| CodeBuddy | `~/.codebuddy/projects` | |
| Factory Droid、Crush、Goose | `~/.factory`、`.crush`、`~/.local/share/goose` | 只记会话累计值，按天的拆分是估算 |

没装的工具不会报错，装上用过以后自动出现。ChatGPT、Claude 桌面版的普通对话、Cursor 等只在云端记录用量的工具读不到；Aider 的本机记录按千取整、Kiro 本机记的 token 多为 0，没有接入。

## 数据从哪来

Agent Desk 默认不联网，只读本机文件：

| 数据 | 来源 |
|---|---|
| token 用量 | `~/.claude/projects`、`~/.codex/sessions`、`~/.gemini/tmp` 下的会话日志 |
| Claude 账号等级 | `~/.claude.json` 里的账号资料（不含凭据） |
| Codex 账号等级 | `~/.codex/auth.json` 里 ID token 的套餐声明，在本机解出，凭据不保存、不发送 |
| Claude 额度 | Claude Code 交给状态栏的 `rate_limits`。在设置里打开「Claude 额度」后，Agent Desk 会把 `~/.claude/settings.json` 的 `statusLine` 包一层脚本，记下额度后照常交给你原来的状态栏命令；改前自动备份，关掉开关就还原。只有终端里的 Claude Code 会运行状态栏。 |
| Codex 额度 | Codex 会话日志里的 `rate_limits` |

只有两个操作会联网，都要手动触发：在「用量」页更新价格表（下载 LiteLLM 的公开 JSON）；在「空间清理」页让 AI 评估（调用本机 `claude` 命令，只发送路径、大小、修改时间和对话标题，不发送文件内容）。

Agent Desk 自己的数据都在 `~/.agent-desk/`：设置、用量缓存、额度记录、停用的技能和 MCP、配置备份。删掉这个目录等于重置。

## 安装

目前需要从源码构建，要求 macOS 12 以上，在 Apple 芯片上测试过。

```bash
pnpm install
pnpm tauri build --bundles app
ditto "target.nosync/release/bundle/macos/Agent Desk.app" "/Applications/Agent Desk.app"
```

应用没有签名，从别处拷来的副本第一次打开时需要在「系统设置 › 隐私与安全性」里放行；自己构建的不需要。

Agent Desk 平时只在菜单栏和屏幕边缘，没有 Dock 图标；在启动台、Spotlight 或「应用程序」里再次打开会显示主面板。退出在菜单栏图标的右键菜单里。

## 开发

需要 Rust（stable）、Node 22、pnpm。

```bash
pnpm install
pnpm tauri dev
```

提交前：

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
pnpm typecheck
```

模块划分和约定见 [docs/CONTRACT.md](docs/CONTRACT.md)。几个 crate 带有 `examples/real.rs`，对本机真实数据只读运行，用来核对结果。

几点注意：

- 构建产物在 `target.nosync`。如果项目放在 iCloud 同步的目录里，建议把它链接到同步范围之外，否则 iCloud 会上传构建产物、清掉本地副本，codesign 也会因扩展属性失败：
  `mkdir -p ~/.cache/agent-desk/target && ln -s ~/.cache/agent-desk/target target.nosync`
  `.npmrc` 把 pnpm 的依赖实体放在 `.pnpm.nosync`，也是这个原因。
- macOS 27 上 release 构建关掉了 strip（见根目录 `Cargo.toml`），否则 dyld 会拒绝加载过程宏动态库。

## 许可

MIT，见 [LICENSE](LICENSE)。安全扫描的部分规则移植自 [ThinkWatch](https://github.com/ThinkWatchProject)（MIT），价格表快照来自 LiteLLM（MIT），详见 [NOTICE](NOTICE)。
