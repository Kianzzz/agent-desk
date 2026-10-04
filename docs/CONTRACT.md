# 模块约定

```
agent-desk/
  crates/ad-usage       用量与费用：读各 AI 客户端的本机日志，按价格表折算
  crates/ad-quota       账号等级与订阅额度，Claude Code 状态栏接入
  crates/ad-inventory   MCP / 技能 / 钩子 / 插件，以及可撤销的开关
  crates/ad-security    只读安全扫描
  crates/ad-disk        磁盘扫描、分级与移到废纸篓
  src-tauri/            Tauri 2 应用外壳（命令、窗口、托盘、后台刷新）
  src/                  React 界面
```

## 接口

每个 crate 的 `src/lib.rs` 放公共类型和对外函数，界面（`src/types.ts`）与之一一对应：

- 改字段的名字、类型或含义时，同步改 `src/types.ts` 和用到它的页面。
- 序列化统一 `#[serde(rename_all = "camelCase")]`，时间统一 RFC 3339 字符串，大小用字节数 `u64`。
- 实现拆成多个模块文件，`lib.rs` 只保留类型和对外函数。

## 共同规则

1. **界面文字用简体中文**，包括 `title`、`detail`、`reason`、`label`、错误信息。写给普通用户看，不堆术语。
2. **不联网**。例外只有两个：`ad-usage` 的价格表更新（LiteLLM 的公开 JSON），`ad-disk` 调用本机 `claude` 命令做 AI 评估（只发送路径、大小、时间、标题这些元数据）。
3. **凭据只在本机用、只取套餐字段**。账号等级从客户端写在本机的账号资料或登录文件里解出（如 Codex ID token 里的套餐声明）；凭据本身不保存、不打印、不发送，也不拿它去调任何接口。
4. **不永久删除任何东西**。删除只移到废纸篓。
5. **密钥不出现在任何输出里**：只给变量名；必须展示时打码成前 4 位 + `…`。
6. **修改用户配置前先备份**到 `<state_dir>/backups/<时间戳>/`，保留原文件的格式和字段顺序（JSON 用 `serde_json` 的 `preserve_order`，TOML 用 `toml_edit`）。
7. **状态目录**：应用传入 `state_dir`（即 `~/.agent-desk`），各 crate 的缓存、停用条目的存档都放在它下面的子目录里。
8. **性能**：`~/.codex/sessions` 这类目录可能有好几 G，单个文件几十 MB，行里可能夹着 base64 图片。只解析需要的行（先看行首的 `type` 字段或用 `memchr` 找关键字，再做 JSON 解析），不要把整个文件读进内存。

## 开发和测试

- 单元测试用 `tests/fixtures/` 里的假数据；修改类操作（开关、移废纸篓、写配置）只在 `tempfile` 临时目录里测。
- 多数 crate 有 `examples/real.rs`，对本机真实的 `$HOME` 只读运行并打印摘要，用来核对结果是否合理。不要在 examples 里调用任何修改类函数。
- 提交前：`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`pnpm typecheck` 全部通过。
