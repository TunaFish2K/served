# served 项目上下文

状态：已确认。本文记录个人非关键服务部署、attach、输出历史和服务安装行为。

## 术语表

- **个人非关键服务**：停止后不会影响主机登录、排查和恢复的个人服务。该术语描述故障
  影响，不描述 CPU、内存或代码规模。
- **部署已有项目目录**：用户先准备项目目录、启动命令和依赖，served 再负责让该命令长期
  运行。代码上传、构建和依赖安装不属于 served 的部署职责。
- **第二屏**：终端备用屏幕，不是 manager 的输出历史，也不是嵌套的终端模拟器。
- **TTY 服务**：配置为 `tty: true`、由 runner 持有 PTY 的服务。它的 attach 会话支持双向
  通信，但同时只有一个写入者。
- **管道服务**：配置为 `tty: false` 的服务。它的 attach 会话只显示实时输出。
- **PTY 尺寸同步**：活动的 TTY attach 客户端把终端 rows 和 cols 应用到 runner 持有的
  PTY。该行为由 `syncRowsCols` 控制，不会回放终端屏幕状态。
- **Attach**：TUI 的 `a` 操作和 `served attach [name]` 命令使用的同一种命名 raw socket
  交接。
- **输出历史**：runner 按每次进程运行记录的 stdout/stderr 或 PTY 输出。它不是 shell
  命令历史，也不能回放成终端屏幕。
- **历史位置**：TUI 显示的从 1 开始的逻辑行位置 `current/total`。它统计清理后的
  `str::lines()` 记录，不统计视觉换行。
- **Attach 快照**：当前运行的清理后输出尾部，只显示最近 48 个逻辑行，最多 16 KiB。
  它不是终端状态回放。
- **持久化日志**：固定 HOME 状态目录下的完整 raw 运行日志文件。非持久化日志是有上限的
  内存运行记录。
- **近期崩溃循环**：runner 在滚动 60 秒内记录到至少 3 次非成功退出，或 worker 启动、
  运行错误。
- **Runner**：隐藏的 `served runner` 进程。它负责一个服务的进程、PTY、输出历史、
  重启循环和私有 runner socket。
- **System service 实例**：可选的 Linux 集成，由共享的 `served@.service` 模板实例化为
  `served@<user>.service`，由 system manager 管理，不是 `systemd --user` unit。
- **安装用户**：运行某个 manager 的普通用户。外部守护程序和该 manager 的所有受管子
  进程都使用该身份；一台主机可以有多个相互隔离的安装用户。
- **固定 HOME 路径**：配置使用 `$HOME/.config`，状态使用 `$HOME/.local/state`。
  `XDG_CONFIG_HOME`、`XDG_STATE_HOME` 和 `XDG_RUNTIME_DIR` 不会改变 served 的路径。
- **安装用户 home**：已安装 system service 使用的规范 home。该用户直接运行
  `served daemon` 时也使用相同的 `$HOME` 路径。主动覆盖 `HOME` 不在支持范围内。
- **旧版 served 环境文件**：`.served.json` 旁边可选的 `.env.served`。它是 dotenv 兼容
  回退输入；项目 `.env` 不属于 served 配置。

## 已确认的决策

- macOS 和 Linux/glibc 都支持 amd64、arm64。每个宿主系统支持本机和同系统另一架构构建，
  不提供跨操作系统构建。外部守护程序以前台 `served daemon` 托管 manager；systemd 只是
  Linux 的可选安装方式。
- 直接 attach 进入备用屏幕，清屏并启用 raw mode。detach、EOF 或错误发生后，恢复 shell
  屏幕和终端模式。
- TUI attach 继续使用 TUI 已持有的备用屏幕。它为服务会话清屏，detach 后完整重绘
  manager，不嵌套第二个备用屏幕所有者。
- 只有 attach 客户端进入备用屏幕。`tty` 仍只控制 PTY 分配；服务启动时 manager 不会
  注入终端控制序列。
- Attach 先显示当前运行的 48 行清理快照，再显示实时输出。快照只用于显示，不会发送给
  服务 PTY。完整输出历史仍由独立的 TUI 历史页和 `served history` 命令提供；历史记录
  不会作为终端状态回放。TUI 保留列表、内容浏览器和位置行。CLI 选择 `latest` 或
  `--run <id>`，用 `-e/--editor`（否则使用 `$EDITOR`）打开持久化 raw 日志；`--path`
  只打印路径。内存记录没有 CLI 路径。
- 管道 attach 转发 stdout/stderr raw 字节，忽略输入，并允许多个观察者。PTY attach
  仍然双向通信，并且只有一个写入者。
- Attach 中的 `Ctrl-C` 只执行 detach，不会把该字节转发给任何服务。
- 只有 attach 失败时才显示面向用户的崩溃循环警告。CLI 和 TUI 使用协议中的结构化诊断，
  有可用的当前持久化 `latest.log` 时通过 `$EDITOR` 提供打开选项。内存历史不会导出到
  临时文件。离开编辑器后不重试 attach。
- 主 TUI 不再显示最近输出面板。无论 TTY 模式如何，都显示 `a attach`。
- `.served.json` 按 JSON5 解析。新的 `served edit` 模板为每个字段添加详细行内注释；
  已有文件打开时不重写。编辑器优先使用 `-e/--editor COMMAND`，否则使用 `$EDITOR`；
  `--path` 创建缺失模板后只打印绝对配置路径。
- 每个服务的环境优先级为 manager 启动环境、旧版 `.env.served` dotenv 值、JSON5 字面量
  `env` 值。新模板不创建 `.env.served`，项目 `.env` 永远不读取。
- `persist_logs` 默认值为 `false`，下次进程启动或重启时生效。持久化日志使用
  `$HOME/.local/state/served/logs/<service>/`，保留 `latest.log` 和 100 个归档，并使用
  `0700`/`0600` 私有权限。
- 每次进程启动都会按 `.latest.started` 旋转已有的 `latest.log`，即使新运行不持久化，
  这样历史模型仍然有一个 `latest` 记录。
- TTY 和管道输出都按 raw 字节捕获。历史显示会移除 ANSI 和其他不安全控制序列，raw
  文件保持不变。
- `syncRowsCols` 默认值为 `true`，下次服务启动或重启时生效。管道服务不会使用它。
- 尺寸控制使用单独的长连接 framed manager connection、opaque attach token 和版本化
  协议。客户端立即发送初始终端尺寸，随后轮询变化；控制连接失败后使用退避重连。detach
  后保留最后的 PTY 尺寸；新建 PTY 使用默认尺寸。
- system service 模板以实例用户身份运行，拒绝 root，不覆盖该用户的主组，并使用其规范
  home 作为登录环境和工作目录。system manager 的 home 展开不属于服务路径约定。
- `/usr/local/bin/served` 和 `served@.service` 是共享文件；每个实例的 socket、registry、
  runner 和服务仍按固定 HOME 路径隔离。共享升级会 handoff 所有活动实例。
- AUR 分为只含二进制的 `served` 和可选的 `served-systemd`；Nix flake 提供 package 和以
  `services.served.users` 声明实例的 NixOS module。其他 init 暂不提供专用包。
- 每个启用服务在 `$HOME/.local/state/served/runtime/runners/<name>/` 下拥有一个 runner。
  manager 失败或收到非预期信号时，不会停止 runner 或服务。明确的 shutdown、disable 和
  服务 restart 会停止它。
- runner 状态包含有上限的内存历史、重启退避和近期崩溃窗口。manager 重启不会清除这些
  状态。已有 attach 流是 manager 代理，在 manager 替换时会断开。

## 兼容性

`Request::Attach { name }` 仍然使用服务名称。manager 根据启用服务配置选择 PTY 或管道
relay。协议版本 3 增加精确的清理后历史行数；版本 4 增加结构化崩溃循环 attach 诊断；
版本 5 增加 manager handoff/shutdown 请求；版本 6 为 handoff 增加目标可执行文件绝对路径，
并增加只退出 manager 的 relinquish 请求。成功 attach 仍返回 opaque token，尺寸控制使用
单独 framed connection 上的 `Request::Resize` 消息。Runner IPC 使用独立的 additive v1
协议，因此 manager 升级可以接管已有 runner。相关握手阶段会拒绝版本不匹配；raw PTY
字节不会与 control frame 混合。

历史请求使用同一 manager IPC，并且需要当前的 manager 二进制。
