# ADR 0012：支持 macOS LaunchDaemon 和统一在线安装

- 状态：已接受
- 日期：2026-08-21

## 背景

served 曾提供 macOS amd64、arm64 二进制，但没有 launchd 安装和升级生命周期。用户需要手动
放置二进制、配置进程守护程序，并自行处理 manager handoff。这不构成完整的平台支持。

Linux 完整包已经使用 systemd system service 托管普通用户 manager，并在共享二进制升级时
保留活动 runner。macOS 支持需要维持相同的用户身份、固定 HOME、多用户隔离和升级保证。

## 决策

- macOS 和 Linux/glibc 都支持 amd64、arm64。macOS 使用 `sysinfo` 的安全接口读取进程启动
  时间；其他运行时、配置、状态和 IPC 语义保持跨平台一致。
- macOS 使用 `/Library/LaunchDaemons/io.github.tunafish2k.served.<uid>.plist`。它是系统级
  LaunchDaemon，但通过 `UserName` 以安装用户身份运行，不依赖图形登录会话。
- `/usr/local/bin/served` 是共享二进制。每个 LaunchDaemon 实例使用自己的规范 HOME、socket、
  registry、runner 和服务。plist 不设置组名，使用账户主组。
- LaunchDaemon 通过安装用户的登录 shell 启动 `served daemon`，设置 HOME 和工作目录，并使用
  `KeepAlive`、30 秒退出超时及 `AbandonProcessGroup`。manager 退出后，独立 runner 不应被
  launchd 当作 manager 进程组的一部分清理。
- macOS 安装器由目标普通用户运行并在内部调用 `sudo`。它使用 `plutil` 渲染和校验 plist，
  记录所有活动 served LaunchDaemon，替换共享文件后逐个执行 manager handoff。
- plist 需要重载时，安装器先 disable 实例并让 manager relinquish，再 bootout、bootstrap、
  enable。新 manager 接管保留的 runner。任何失败都会恢复旧文件和原活动实例。
- macOS 卸载只移除当前用户实例并保留用户数据。其他实例存在时保留共享二进制；没有其他
  实例时，再单独确认是否删除二进制。
- Linux 和 macOS 共用在线引导脚本。脚本检测系统与架构，解析 GitHub 最新稳定 Release，
  下载 full 包及 SHA-256 sidecar，校验后以 `--yes` 调用包内安装器。重复运行同一命令即升级。
- 不增加 Rust 自更新命令，也不创建自动更新任务。非 systemd Linux 继续使用二进制包和自选
  supervisor。
- macOS Release 提供 binary 和 full 包。二进制使用 ad-hoc 签名，不做 Developer ID 签名或
  notarization；amd64 deployment target 为 10.12，arm64 为 11.0。

## 结果

macOS 成为完整发行平台，而不只是可编译目标。用户可以通过一个命令安装或升级，并获得开机
运行、多用户隔离、runner 保留和失败回滚。

LaunchDaemon 需要 `sudo`。macOS 隐私保护仍可能限制它访问 Desktop、Documents 等目录；用户
需要授予 Full Disk Access，或把服务目录放在不受保护的位置。ad-hoc 签名也不提供 notarized
应用的 Gatekeeper 体验。
