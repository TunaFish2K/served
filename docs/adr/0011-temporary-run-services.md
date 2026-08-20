# ADR 0011：无配置临时服务

- 状态：已接受
- 日期：2026-08-20

## 背景

已启用服务要求项目目录提供 `.served.json` 和启用链接。临时测试和短期任务也可能需要
PTY、attach、历史、重启策略和 manager 接管。用户不应为了使用这些功能而修改项目目录。

## 决策

- 新增 `served run [options] -- <program> [args...]`。该命令忽略项目中的 `.served.json` 和
  `.env.served`。它使用 manager 环境快照，并应用显式 `--env` 覆盖。该命令创建临时服务。
- `--` 后的参数保持 argv 边界。manager 对每个参数执行 POSIX 安全引用，然后复用现有
  `/bin/sh -c` worker。用户必须显式传入 `sh -c` 才能使用 shell 语法。
- 临时服务不写启用注册表。runner runtime 目录保存版本化的私有 runtime 描述。该文件的
  权限是 `0600`。manager handoff、relinquish 或异常重启后使用该描述验证并接管活动 runner。
- 服务进程退出后，临时服务保持 `stopped` 状态，直到用户运行 `served disable`。正常
  shutdown、正常停止 manager 或没有活动 runner 的恢复路径会删除私有 runtime 描述。
  这些路径不会自动启动服务。
- 同一名称或目录只能有一个受管服务。已启用服务和临时服务共用 list、TUI、attach、
  history、restart、disable、runner 和日志实现。
- 公共 manager 协议升级为 v7，增加 `Run` 请求和 `enabled | temporary` 服务类型。runner
  协议保持 additive v1，已有 runner wire shape 不变。

## 结果

用户不创建配置文件也可以使用服务管理功能。项目配置仍只描述需要持续启用的服务。私有
runtime 描述只支持跨 manager 接管活动 runner。它不定义主机重启后的期望状态。
