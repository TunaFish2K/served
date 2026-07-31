# ADR 0005：为安装用户使用 system service

- 状态：已接受
- 日期：2026-07-25

## 背景

管理器必须在 SSH 会话结束后继续运行，同时受管进程应保持普通用户身份，并由一个安装
用户拥有。之前的 user-service 方案需要启用 lingering，还依赖会话范围的 runtime 路径；
直接运行 `served daemon` 也容易与已安装管理器使用不同配置。

## 决策

- 为主机安装一个固定的 `/etc/systemd/system/served.service` unit。
- unit 使用 `User=` 和 `Group=` 设置安装用户身份；管理器永远不是 root daemon。
- unit 为 `multi-user.target` 启用，并设置 `Restart=always`、`RestartSec=1s` 和
  `NoNewPrivileges=yes`。
- 使用安装用户的 login 环境和 home 作为 unit 的环境和工作目录，再通过 `/bin/sh -lc`
  启动 `/usr/local/bin/served daemon`，以便在管理器启动时读取安装用户的 profile。unit
  不得使用 system manager 的 `%h` specifier 生成这些路径；在 system instance 中，它可能
  解析为 manager 的 home，而不是 `User=` 用户的 home，导致管理器启动前就在 `CHDIR`
  阶段失败。
- 所有配置、状态和 socket 路径只从 `HOME` 派生：`~/.config`、`~/.local/state` 和
  `~/.local/state/served/runtime/served.sock`。`XDG_*` 变量不选择 served 路径。
- 安装器由目标用户运行，并在内部调用 `sudo`。如果 system unit 属于其他用户，安装器
  拒绝覆盖。
- 只有得到确认后才迁移旧的 `systemd --user` 安装。旧 user manager 必须可访问；否则
  迁移中止且不删除旧文件。只有新的 system service active 后，才删除旧文件。

## 考虑过的方案

- 保留 `systemd --user` 并启用 lingering：拒绝，因为服务生命周期和 runtime socket 会
  依赖用户 manager 与会话行为。
- 让 system unit 以 root 运行：拒绝，因为这会扩大权限边界，而 served 的宿主机用户进程
  模型不需要 root。
- 使用 systemd template unit 支持多个用户：V1 拒绝。一个主机只支持一个安装用户和一个
  固定服务名。
- 继续使用 XDG runtime/config/state 变量：拒绝，因为直接 daemon 和已安装服务可能解析
  到不同位置。
- 使用 system manager 的 `%h` 生成 `HOME` 和 `WorkingDirectory`：拒绝，因为它不能可靠
  指向 system unit 的 `User=` 账户。
- 安装时把绝对 home 路径写入 unit：拒绝，因为这会复制 home 来源，账户 home 移动后需要
  重新安装。

## 结果

管理器在 logout 后仍然运行，手动启动和 systemd 启动使用同一个稳定 socket 路径。系统安装
需要 `sudo`，每台主机只支持一个安装用户。现有自定义 XDG 数据不会自动移动。迁移旧 user
service 时，需要可访问的 user manager，以便脚本安全地停止和 disable 它。升级会保留 inactive
或 failed 服务的停止状态，并报告启动所需命令，不会悄悄改变 enabled 状态。
