# served 技术栈

状态：实现基线。

本文记录 V1 的实现选择。产品面向个人开发者在 macOS 或 Linux 主机上部署个人非关键服务。
每个 manager 只服务一个普通用户，但同一主机可以运行多个相互隔离的 manager。“极简”
表示操作面和所有权边界简单，不表示依赖数量绝对最少。

served 管理已经存在的项目目录和启动命令。它不负责代码上传、构建、依赖安装或健康检查。
前台 manager 由用户选择的进程守护程序托管。仓库提供可选的 Linux systemd system
service 和 macOS LaunchDaemon，但不替代宿主进程守护程序，也不提供容器隔离、root 服务
管理或资源限制。

## 运行时

| 领域 | 选择 | 边界 |
| --- | --- | --- |
| 语言 | Rust stable | 第一方代码使用 `#![forbid(unsafe_code)]` |
| 构建 | Cargo，提交 `Cargo.lock` | 一个 package，一个 `served` 二进制 |
| 异步运行时 | Tokio | Unix socket 服务、进程监督和定时器 |
| 管理模型 | Manager actor，以及每个受管服务一个独立 runner 进程 | 管理器持有状态缓存，并负责 IPC 和注册表。runner 负责子进程生命周期和输出 |
| Worker 模型 | 一套事件驱动 supervisor，外加 PTY 和 pipe 适配器 | 统一处理退出、重启、stop、attach、resize、进程组终止和输出分发 |
| 服务命令 | `/bin/sh -c <command>` | 工作目录是服务目录 |
| 无 PTY 的进程 | `tokio::process` 和异步管道 | `.served.json` 使用 `tty: false` 时启用 |
| 有 PTY 的进程 | `portable-pty` | 默认路径；master 始终由 worker 持有 |
| 进程身份 | Linux `/proc`；macOS `sysinfo` | Linux 保留既有 tick 格式；macOS 使用安全 API 的启动时间 |
| 本地时间戳 | `chrono` | 为归档日志生成可读的本地运行名称 |

管理器以前台进程运行。外部守护程序必须使用安装用户身份、规范 home 和固定 HOME 路径。
通用生命周期接口是 `served daemon`、`served shutdown`、`served daemon --handoff` 和
迁移专用的 `served daemon --relinquish`。可选 systemd 模板使用 `User=%i` 和
`KillMode=process`，不设置 `Group=`，并拒绝 root 实例。macOS LaunchDaemon 使用
`UserName`、规范 HOME、登录 shell、`KeepAlive` 和 `AbandonProcessGroup`。两种集成都在升级
时 handoff 活动 manager；需要更换 supervisor 配置时先 relinquish，再由新实例接管 runner。

## 配置与状态

- `json5` 通过 `serde` 反序列化带注释、直接对象形式的 `.served.json`；IPC 消息仍使用
  `serde_json` 作为 wire format。
- `dotenvy` 按 dotenv 规则解析固定的旧版 `.env.served` 回退文件。文件按数据读取，永远
  不会由 shell source。
- 管理器捕获启动环境，再叠加旧版 `.env.served`，最后为每个服务叠加 JSON5 字面量 `env`。
- 启用注册表是 `$HOME/.config/served/enabled/<name>` 下的符号链接。
- 临时服务不写启用注册表。私有 runtime 描述位于 runner 目录中。manager 只在 runner
  仍活动时恢复临时服务。
- 可选服务历史存储在 `$HOME/.local/state/served/logs/<name>` 下。持久化运行记录是完整
  raw 文件；内存运行记录每次保留 64 KiB 尾部。
- 活动持久化文件为 `latest.log`；`.latest.started` 保存其开始标签，旧 latest 文件会被
  重命名为带时间戳的归档。
- V1 不使用 manager 状态 JSON，也不持久化 tips 游标。

## IPC

- 控制命令使用 `$HOME/.local/state/served/runtime/served.sock`。
- frame 通过 `tokio-util` 传输带长度前缀的 JSON 消息。
- 每个连接先执行协议版本握手。
- transport、公共 manager wire DTO 和私有 runner wire DTO 分层。日志和运行时内部类型不会
  直接充当 wire DTO。
- 历史读取使用管理器代理的运行器列表请求和分页 chunk 读取。因此客户端不需要直接访问
  状态目录，较大的文件也不会超过单个 frame。协议版本 3 为每个历史 chunk 增加精确的
  清理后逻辑行数，用于 TUI 位置行。管理器协议版本 5 增加结构化崩溃循环 attach 诊断、
  handoff 和 shutdown。版本 6 为 handoff 增加目标可执行文件路径，并增加 relinquish。
  当前版本 7 增加 `Run` 请求和服务类型。
- 运行器 IPC 使用独立的 additive v1。新 runner 支持 `WatchStatus` 长连接。连接先发送当前
  状态，再发送变化。manager 对旧 v1 runner 自动退回每秒一次的 `Status` 查询。列表请求
  只读取 manager 缓存，不在请求路径中逐个探测 runner。
- Attach 会把已经认证的连接切换为 raw 字节流。PTY 服务使用一个双向 writer；管道服务
  将 raw stdout/stderr 广播给多个只读观察者，并丢弃它们的输入。每个 attach relay 先从
  运行器取得当前运行的清理快照，再接收实时输出。快照为 48 个逻辑行，最多 16 KiB。
  Attach 尺寸控制使用另一条长连接 framed connection 和 opaque token，因此终端尺寸消息
  不会与 raw 字节流混在一起。
- socket 使用只允许用户访问的文件权限创建，不通过 TCP 暴露。

## CLI 与 TUI

- `clap` derive 定义单二进制命令界面。
- `ratatui` 和 `crossterm` 绘制服务列表、attach 切换和历史浏览器；配置编辑明确交给
  用户的外部编辑器。
- 共享编辑器模块使用 `tokio::process` 和 `/bin/sh -c`，将安全引用的配置或日志路径作为
  最后一个参数追加。所有编辑入口依次使用显式 `-e/--editor`、`$EDITOR`，或从 `PATH`
  查找 `editor`、`sensible-editor`、`nvim`、`vim`、`vi`、`nano`、`micro`、`hx`。
- `rand` 用于每次 TUI 启动时非加密地随机选择 tips。
- TUI 首屏是全局受管服务列表。它显示服务类型和状态，也提供 restart、disable、attach 和
  history。首屏只显示一行 `tips:`。上下文操作栏始终位于 tips 下方。窄终端使用两行
  操作栏。PTY 和管道服务都显示 attach。
- `served edit` 只在 `.served.json` 缺失时创建带注释的 JSON5 模板，再直接打开文件。已有
  源文本保持不变；`--path` 只创建模板和报告路径，不启动编辑器。
- `h` 历史页面先列出记录，再分页加载清理后的内容。页面显示简单的 `current/total`
  逻辑行位置，保留轮换的 `tips:`，不会通过 attach 回放历史。
- `served history [name]` 默认选择 `latest`，`--run <id>` 选择归档。编辑器或 `--path`
  访问持久化 raw 日志；`--stdout` 流式输出清理后的内容，`--json` 输出内容和记录元数据。
  两者都通过分页 IPC 支持持久化和内存记录，不创建临时文件。
- `served attach [name]` 复用基于名称的 raw socket 交接。不提供名称时，客户端根据规范化
  的当前目录和管理器启用列表解析服务。直接 attach 使用 crossterm raw mode，并在退出前
  持有备用屏幕。
- TUI attach 复用 TUI 已持有的备用屏幕。它在 raw relay 前后清屏和重绘，不嵌套第二个备用
  屏幕所有者。
- 每个运行器按服务维护滚动 60 秒失败队列。三次失败会产生协议版本 5 的 attach-unavailable
  诊断，并在可用时附带持久化 `latest.log` 路径。CLI 和 TUI 只在 attach 失败后询问；TUI
  运行统一解析出的编辑器时会暂时离开 raw mode 和备用屏幕，结束后恢复两者。
- Attach 客户端立即读取 `crossterm::terminal::size()` 并发送初始尺寸，之后约每 250ms
  检查一次，只发送变化。控制失败时使用退避重试，raw attach 继续运行。只有
  `syncRowsCols` 和 attach token 都有效时，管理器才调整活动 worker PTY。
- 共享 attach relay 在直接 attach 和 TUI attach 中都把 `Ctrl-C` 作为 detach，不转发给服务。
  管道 attach 忽略其他输入。Attach 快照只显示清理后的输出，不回放终端控制状态。

## 错误、日志与测试

- 应用边界使用 `anyhow`。
- 使用 `thiserror` 定义便于调用方和测试处理的错误。
- `tracing` 与 `tracing-subscriber` 将管理器生命周期事件记录到 stderr。systemd 写入 journal；
  LaunchDaemon 写入用户 state 目录的 manager 诊断日志。
- 单元测试覆盖 JSON5 配置、旧版 dotenv 覆盖、注册表、协议 frame、崩溃窗口、日志轮换、
  历史分页、增量输出清理、重启退避、运行器状态订阅、旧 v1 runner 轮询回退和运行器生命周期
  消息。集成测试覆盖管理器崩溃接管、manager handoff 和 relinquish，以及显式 shutdown。
- Linux shell 检查使用当前用户渲染服务模板，并运行 `systemd-analyze verify`；同时防止
  重新引入用于 home 路径的 `%h`。
- launchd 检查验证 plist 必需字段，并在 macOS 使用 `plutil` 检查语法。在线安装器测试通过
  mock GitHub 下载验证平台选择、checksum gate 和 `--yes` 调用。
- 集成测试使用 `tempfile` 和 `assert_cmd`；平台 release smoke test 在环境允许时额外测试
  真实 system service 或 LaunchDaemon 安装。
- TUI 渲染测试使用 Ratatui 的 `TestBackend`。第一阶段不要求快照测试。

## 构建与打包

- 推送匹配的 `v<semver>` tag 后，GitHub release workflow 自动运行。
- `Makefile` 统一本机构建、测试、隔离运行、同系统跨架构编译和 Docker Linux 检查。
- macOS 使用 Rust/Clang 构建 amd64、arm64，deployment target 分别为 10.12、11.0。
- Linux 使用 Zig 0.14.1 和 cargo-zigbuild 0.21.8 构建 amd64、arm64，固定 glibc 2.17。
- CI 在 macOS 和 Linux 的 amd64、arm64 原生 runner 上运行测试，并从每种宿主架构构建同一
  系统的另一架构。
- Linux 完整安装包包含 systemd 集成；macOS 完整安装包包含 LaunchDaemon plist 和对应
  安装、卸载脚本。macOS 二进制使用 ad-hoc 签名，不做 notarization。
- 每个产物都有自己的 SHA-256 sidecar，命名方式是在原文件名后追加 `.sha256`。
- Release 额外生成确定性的 source archive，作为通用源码发行产物。
- 完整安装包包含平台二进制、对应 supervisor 模板、安装器、卸载器和 README。
- 仓库不维护发行版包管理器元数据或模块；外部包不属于项目的兼容和发布 gate。
- shell 脚本负责共享文件安装、所有活动实例 handoff、systemd/launchd 生命周期和失败回滚。
  Rust 不调用 `systemctl`、D-Bus 或 `launchctl`。
- POSIX 在线脚本从 GitHub 最新稳定 Release 下载平台 full 包和 SHA-256 sidecar，校验后调用
  包内 `install.sh --yes`。它不实现 CLI 或后台自动更新。
- V1 支持 macOS 和 Linux/glibc 的 amd64、arm64，不支持跨操作系统构建、musl 或 Windows。

## 依赖策略

依赖集合保持常规和成熟。新增 crate 必须消除有意义的复杂度，或满足明确的边界需求；
不能只为了封装几行本地代码而添加。
