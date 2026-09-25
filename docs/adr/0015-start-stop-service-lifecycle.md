# ADR 0015：保留注册和历史的 start / stop

- 状态：已接受；run 服务的冷启动恢复行为由 [ADR 0016](0016-persistent-run-services.md) 更新。
- 日期：2026-09-23
- 部分取代：V1 无独立 start/stop 命令的边界；ADR 0008、0011、0014 的恢复规则

## 决策

CLI 提供 `start [name]`、`stop [name]`，TUI 提供 `s start`、`x stop`。沿用名称或当前
工作目录定位，不接受 `-f`，不注册未知服务。已停止时 stop 成功返回；运行、启动或自动
重启退避中 start 成功返回，不读取修改后的配置、不改变进程。

start 启动停止或失败服务。已启用服务重新加载注册来源并校验，失败保持停止；临时服务
复用原始命令、选项和环境。restart 也能解除手动停止。start/restart 开始新的自动重启周期。

stop 取消自动重启，停止进程组并确认 worker 完成；它保留 runner、身份元数据、注册和
历史。停止后 attach 断开、PID 为空，history 可读。disable/shutdown 继续完整关闭 runner。
停止失败返回错误并保留控制能力。worker 快速退出导致回复通道关闭时，只有成功等待
worker 任务结束才可认为已完成；不同 worker 使用独立事件通道，防止旧事件覆盖新状态。
等待停止时同时消费输出，避免有界事件队列阻塞终止。

## 恢复和兼容

手动停止由 runner 保存，不写永久停止标记。handoff、relinquish 和 manager 崩溃后接管
存活 runner 时保留停止状态及已加载定义，不应用编辑后的磁盘配置，即使新配置无效。
manager 存活时重建故障 runner 保留已知停止意图；manager 和 runner 同时丢失则不承诺
恢复该意图。正常 shutdown 后完整启动 manager 或主机重启时，已启用服务自动启动，
临时服务不恢复。

manager 协议升至 v9，增加 Start/Stop。runner 协议保持 additive v1，增加 StartService、
StopService、ConfigureStopped；最后一个请求只用于初始化未配置的新 runner 为停止状态。
原 Stop 仍表示完整关闭。status 新增 supports_start_stop、manually_stopped，缺省为 false，
为 false 时省略序列化，旧 wire 形状保持可读。

旧 runner 不支持新命令时明确报错，不能自动替换或退回旧 Stop。用户可 disable 后按原
配置来源和工作目录覆盖重新 enable，临时服务则按原命令、选项和环境重新 run。
迁移提示必须说明内存历史会丢失、持久化日志保留。

## handoff 后的进程回收

handoff 使用 exec，原 manager 的 Tokio 子进程等待任务不会保留。新 manager 在创建任何
runner 前注册 SIGCHLD 并捕获已有 runner 的 PID 与启动时间，立即检查并在退出信号到来时
按具体 PID 非阻塞回收。非当前进程的子进程按 ECHILD 移出跟踪；新创建的 runner 继续由
Tokio Child 等待任务管理。避免使用 waitpid(-1)，以免抢走其他子进程的退出状态。

僵尸或已死亡进程不计为存活。可查询的启动时间用于回收前身份验证；macOS 可能不再
暴露已退出进程的信息，此时使用已记录身份并由特定 PID 的 waitpid 判断子进程归属。
查询到不匹配的启动时间时放弃回收，避免误认复用的 PID。
Linux 的暂停、不可中断睡眠状态仍计为存活；macOS sysinfo 的 Dead 表示线程不可中断睡眠，
不能将其当作进程已退出。

## 验证

测试覆盖 PTY/pipe、幂等调用、配置加载时机、内存历史、attach 断开、自动重启取消、
快速退出竞态、停止失败、manager 接管、runner 重建、完整重启恢复，以及旧 runner
在拒绝新操作时不会收到任何变更请求。新增操作最初通过换行页脚展示；后续
[无框 TUI 设计规范](../TUI-DESIGN.md) 改用 Enter 菜单和 ? 帮助，保留原快捷键。
