# ADR 0008：使用独立运行器恢复管理器

- 状态：已接受
- 日期：2026-07-31

## 背景

管理器是 IPC 和 TUI 入口，但不应成为受管服务的生命周期边界。管理器崩溃、`SIGKILL`、
systemd 重启或二进制 handoff 都不能终止仍然健康的服务。同时，显式 shutdown、disable
或服务 restart 仍然必须确定地停止服务。

## 决策

- 单个 `served` 二进制包含隐藏的 `served runner` 模式。每个启用服务恰好由一个运行器进程
  持有。
- 运行器负责服务 shell 或 PTY、重启退避、崩溃循环窗口、attach 状态、输出分发和
  `LogStore`。它的私有 socket 和元数据位于
  `$HOME/.local/state/served/runtime/runners/<name>/`。
- 管理器负责启用注册表和公共 IPC。它通过私有协议接管运行器，并代理 attach 和历史操作；
  管理器不直接持有服务进程。
- 管理器启动时比较当前加载的服务规格与运行器规格。规格变化会作为受控运行器重启应用。
  如果运行器仍存活但控制 socket 暂不可用，不会重复启动；必须等到它的身份不再可观察后
  才能恢复。
- 管理器公共协议为版本 6。运行器 IPC 是独立的版本 1 协议，因此 manager handoff 可以
  替换管理器而不替换运行器。
- `systemctl reload served@<user>` 请求 manager handoff。请求包含客户端当前可执行文件的
  绝对路径；管理器验证它是可执行的普通文件，关闭公共 listener，并执行该路径。运行器
  继续运行。system unit 使用 `KillMode=process`，因此管理器失败或替换时，systemd 不会
  清理运行器。
- `served daemon --relinquish` 让管理器释放公共 socket 并以状态 75 退出，但不停止 runner。
  systemd 模板把 75 同时标记为成功和禁止自动重启，用于从旧 unit 转移到新 unit；新
  supervisor 随后启动 manager 并接管 runner。
- `served shutdown`、`served disable` 和 `served restart` 是明确的服务生命周期操作。它们
  向相关运行器发送停止请求，并等待服务进程被 reap 后再删除运行器状态。
- 管理器的 attach 代理连接可能在管理器崩溃或 handoff 时丢失。服务和运行器仍然存活，
  新管理器启动后可以再次 attach。

## 结果

管理器可以重启或升级而不中断健康服务，内存历史和崩溃诊断也能跨管理器替换保留。代价是
每个启用服务多一个进程和一套私有 IPC 协议。

如果运行器身份已经消失，但服务 PID 仍然存活，管理器不能安全重建运行器。此时管理器会
报告运行器不可用，而不是重复执行命令。显式生命周期命令仍然是可靠的清理路径。
