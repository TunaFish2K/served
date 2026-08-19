# ADR 0010：事件驱动状态与统一运行时边界

- 状态：已接受
- 日期：2026-08-09

## 背景

v0.4.1 已经定义完整的 CLI、TUI、manager v6 协议、runner v1 协议和服务生命周期行为。
重写必须保持这些外部行为，同时解决三个维护问题：列表请求会同步探测所有 runner，PTY 和
pipe 的监督路径重复处理生命周期，transport、wire DTO 和内部存储类型的所有权不够清楚。

manager 升级还必须接管由旧二进制启动的 runner。runner v1 因此不能通过不兼容的版本提升
来获得事件推送。

## 决策

- 公共 manager 协议保持 v6，现有请求、响应和 JSON shape 不变。
- runner 协议保持 additive v1。新增 `WatchStatus`：连接先收到当前状态，之后只在状态变化
  时收到更新。旧 v1 runner 关闭或拒绝该请求时，manager 自动退回每秒一次的 `Status` 查询。
- manager 为每个 runner 维护带 generation 的 watcher 和状态缓存。`List` 只读取缓存；旧
  watcher 的迟到消息不能覆盖替换后的 runner 状态。
- worker 只有一套 supervisor 状态机。PTY 和 pipe 分别负责启动及 I/O 适配，共享进程退出、
  stop、restart、attach、resize、输出快照与广播、进程组 TERM/KILL 和清理规则。
- `ipc` 只负责 framed Unix transport 和 raw handoff。`protocol` 拥有公共 manager wire
  DTO，`runner_protocol` 拥有私有 runner wire DTO，日志模块拥有内部历史记录。
- CLI、TUI model/view/effect、manager daemon/watcher、runner server 和日志显示分别进入独立
  模块。Rust 源码模块路径不是兼容承诺；CLI、TUI、配置、路径、wire 和运行时行为才是。

## 结果

列表请求的成本不再随 runner socket 往返线性增长。新 runner 的状态变化会立即进入 manager
缓存，旧 runner 仍可被新 manager 接管。PTY 和 pipe 不再拥有两套生命周期实现，协议 DTO
也不会因内部状态重构而隐式改变。

状态 watcher 断开时，manager 会标记该 runner 不可用并走既有恢复流程。旧 runner 回退仍有
一秒轮询延迟，这是兼容既有 v1 进程的有界成本，不是新运行路径。
