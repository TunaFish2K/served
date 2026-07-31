# served 技术栈

状态：实现基线。

本文记录 V1 的实现选择。产品面向个人开发者在 Linux 主机上部署个人非关键服务，并使用
一个安装用户。“极简”表示操作面和所有权边界简单，不表示依赖数量绝对最少。

served 管理已经存在的项目目录和启动命令。它不负责代码上传、构建、依赖安装或健康检查。
它使用 systemd system service 保证管理器在用户退出 SSH 后继续运行。它不替代 systemd，
也不提供容器隔离、root 服务管理或资源限制。

## 运行时

| 领域 | 选择 | 边界 |
| --- | --- | --- |
| 语言 | Rust stable | 第一方代码使用 `#![forbid(unsafe_code)]` |
| 构建 | Cargo，提交 `Cargo.lock` | 一个 package，一个 `served` 二进制 |
| 异步运行时 | Tokio | Unix socket 服务、进程监督和定时器 |
| 管理模型 | Manager actor，以及每个启用服务一个独立 runner 进程 | 管理器负责 IPC/注册表；运行器负责子进程生命周期和输出 |
| 服务命令 | `/bin/sh -c <command>` | 工作目录是服务目录 |
| 无 PTY 的进程 | `tokio::process` 和异步管道 | `.served.json` 使用 `tty: false` 时启用 |
| 有 PTY 的进程 | `portable-pty` | 默认路径；master 始终由 worker 持有 |
| 本地时间戳 | `chrono` | 为归档日志生成可读的本地运行名称 |

管理器作为固定的 `served.service` system unit 运行。unit 使用 `User=` 和 `Group=` 设置
安装用户身份。管理器和独立运行器都留在该 unit 的 cgroup 中。`KillMode=process` 让管理器
重启时可以保留运行器和受管子进程，供新管理器接管。`served` 不会变成 root daemon、容器
运行时、namespace 管理器或资源策略引擎。unit 通过 `/bin/sh -lc` 加载安装用户的 profile，
使用 `SetLoginEnvironment=yes`，并设置 `WorkingDirectory=~`，让 system manager 解析安装
用户的 home。它不使用 system manager 的 `%h` specifier。

## 配置与状态

- `json5` 通过 `serde` 反序列化带注释、直接对象形式的 `.served.json`；IPC 消息仍使用
  `serde_json` 作为 wire format。
- `dotenvy` 按 dotenv 规则解析固定的旧版 `.env.served` 回退文件。文件按数据读取，永远
  不会由 shell source。
- 管理器捕获启动环境，再叠加旧版 `.env.served`，最后为每个服务叠加 JSON5 字面量 `env`。
- 启用注册表是 `$HOME/.config/served/enabled/<name>` 下的符号链接。
- 可选服务历史存储在 `$HOME/.local/state/served/logs/<name>` 下。持久化运行记录是完整
  raw 文件；内存运行记录每次保留 64 KiB 尾部。
- 活动持久化文件为 `latest.log`；`.latest.started` 保存其开始标签，旧 latest 文件会被
  重命名为带时间戳的归档。
- V1 不使用 manager 状态 JSON，也不持久化 tips 游标。

## IPC

- 控制命令使用 `$HOME/.local/state/served/runtime/served.sock`。
- frame 通过 `tokio-util` 传输带长度前缀的 JSON 消息。
- 每个连接先执行协议版本握手。
- 历史读取使用管理器代理的运行器列表请求和分页 chunk 读取。因此客户端不需要直接访问
  状态目录，较大的文件也不会超过单个 frame。协议版本 3 为每个历史 chunk 增加精确的
  清理后逻辑行数，用于 TUI 位置行。当前管理器协议版本 5 增加结构化崩溃循环 attach
  诊断，以及生命周期 handoff/shutdown 请求；运行器 IPC 使用独立的 additive v1。
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
  最后一个参数追加。配置和历史命令的 `-e/--editor` 优先于 `$EDITOR`；attach 崩溃诊断
  只使用 `$EDITOR`。
- `rand` 用于每次 TUI 启动时非加密地随机选择 tips。
- TUI 首屏是全局启用服务列表。它显示状态，并提供 restart、disable、attach、history 和
  唯一的一行 `tips:`。上下文操作栏始终位于 tips 下方，窄终端时换成两行；PTY 和管道
  服务都显示 attach。
- `served edit` 只在 `.served.json` 缺失时创建带注释的 JSON5 模板，再直接打开文件。已有
  源文本保持不变；`--path` 只创建模板和报告路径，不启动编辑器。
- `h` 历史页面先列出记录，再分页加载清理后的内容。页面显示简单的 `current/total`
  逻辑行位置，保留轮换的 `tips:`，不会通过 attach 回放历史。
- `served history [name]` 默认选择 `latest`，使用 `-e/--editor` 或 `$EDITOR` 打开持久化
  raw 日志；`--run <id>` 选择归档，`--path` 只打印路径。内存记录可以在 TUI 查看，但
  没有 CLI 文件路径。
- `served attach [name]` 复用基于名称的 raw socket 交接。不提供名称时，客户端根据规范化
  的当前目录和管理器启用列表解析服务。直接 attach 使用 crossterm raw mode，并在退出前
  持有备用屏幕。
- TUI attach 复用 TUI 已持有的备用屏幕。它在 raw relay 前后清屏和重绘，不嵌套第二个备用
  屏幕所有者。
- 每个运行器按服务维护滚动 60 秒失败队列。三次失败会产生协议版本 5 的 attach-unavailable
  诊断，并在可用时附带持久化 `latest.log` 路径。CLI 和 TUI 只在 attach 失败后询问；TUI
  运行 `$EDITOR` 时会暂时离开 raw mode 和备用屏幕，结束后恢复两者。
- Attach 客户端立即读取 `crossterm::terminal::size()` 并发送初始尺寸，之后约每 250ms
  检查一次，只发送变化。控制失败时使用退避重试，raw attach 继续运行。只有
  `syncRowsCols` 和 attach token 都有效时，管理器才调整活动 worker PTY。
- 共享 attach relay 在直接 attach 和 TUI attach 中都把 `Ctrl-C` 作为 detach，不转发给服务。
  管道 attach 忽略其他输入。Attach 快照只显示清理后的输出，不回放终端控制状态。

## 错误、日志与测试

- 应用边界使用 `anyhow`。
- 使用 `thiserror` 定义便于调用方和测试处理的错误。
- `tracing` 与 `tracing-subscriber` 将管理器生命周期事件记录到 stderr 和 systemd journal。
- 单元测试覆盖 JSON5 配置、旧版 dotenv 覆盖、注册表、协议 frame、崩溃窗口、日志轮换、
  历史分页、重启退避和运行器生命周期消息。集成测试覆盖管理器崩溃接管、manager handoff
  和显式 shutdown。
- Linux shell 检查使用当前用户渲染服务模板，并运行 `systemd-analyze verify`；同时防止
  重新引入用于 home 路径的 `%h`。
- 集成测试使用 `tempfile` 和 `assert_cmd`；Linux release smoke test 在环境允许时额外
  测试真实 system service 安装。
- TUI 渲染测试使用 Ratatui 的 `TestBackend`。第一阶段不要求快照测试。

## 打包

- 推送匹配的 `v<semver>` tag 后，GitHub release workflow 自动运行。
- Release 完整安装包是个人部署的首选入口。安装用户运行其中的 `install.sh`，脚本在需要时
  调用 `sudo` 安装 system service。
- workflow 面向 Linux amd64/glibc，发布只含二进制的产物和完整离线安装包。
- 每个产物都有自己的 SHA-256 sidecar，命名方式是在原文件名后追加 `.sha256`。
- 完整安装包包含 glibc 链接的二进制、system unit 模板、`install.sh`、`uninstall.sh` 和
  `README.md`。
- shell 脚本负责 system unit 安装、`daemon-reload`、enable/start 和旧 user-service 迁移。
  Rust 不调用 `systemctl` 或 D-Bus。
- V1 首先支持 Linux/glibc，不承诺其他平台兼容。

## 依赖策略

依赖集合保持常规和成熟。新增 crate 必须消除有意义的复杂度，或满足明确的边界需求；
不能只为了封装几行本地代码而添加。
