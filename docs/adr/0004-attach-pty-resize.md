# ADR 0004：Attach 同步 PTY 窗口尺寸

- 状态：已接受
- 日期：2026-07-25

## 背景

PTY 进程启动时使用运行器的默认尺寸。Vim 这样的终端应用会从 PTY 读取尺寸，因此从
不同尺寸的终端 attach 时，应用可能按错误的行列数渲染。

现有 raw attach 流已经是双向字节交接，不能把管理器控制消息安全地混入用户输入和服务
输出中。

## 决策

- 在 `.served.json` 中增加 `syncRowsCols`，对已有和新配置默认 `true`。带注释的 JSON5
  模板解释该字段。`tty: false` 时字段仍保存，但不生效。
- 编辑字段只会写入配置。值在下一次服务启动或重启时生效；`served edit` 不重启运行中的
  服务。
- 管理器协议版本提升到 2。之后历史逻辑行元数据提升到 3，结构化崩溃循环 attach 诊断
  提升到 4，管理器生命周期 handoff/shutdown 将当前协议提升到 5。本 ADR 记录最初需要
  版本 2 的 attach/resize 变更。Resize 请求包含服务名、token 以及正数 `cols`/`rows`。
  协议不匹配会在已有握手阶段失败。
- 尺寸控制使用单独的长连接 framed manager connection。Attach 客户端立即发送当前终端
  尺寸，随后约每 250ms 轮询 `crossterm::terminal::size()`，只发送变化。
- 控制连接失败不会终止 raw attach。客户端使用退避重连，并在重连后重新发送当前尺寸。
- 只有运行中的 TTY 服务、匹配的活动 attach token 和启用的同步设置同时满足时，运行器才
  应用有效尺寸。管道服务、旧 token、关闭同步和不存在的 attach 会成功但不产生操作；
  缺少服务、服务停止或尺寸为 0 时返回错误。
- Detach 后保留最后的 PTY 尺寸。新建 PTY 使用默认尺寸。

## 考虑过的方案

- 把 resize frame 放进 raw attach 流：拒绝，因为任意 PTY 输入/输出字节必须保持透明，并
  与交互式程序兼容。
- 每次尺寸变化都建立新的 manager connection：拒绝，因为会增加连接开销，也让顺序和重连
  行为更难分析。
- 使用终端 resize 信号或第二个 stdin reader：拒绝，因为客户端可以轮询尺寸，不需要与
  现有 raw 输入循环竞争。

## 结果

交互式 TUI 程序可以使用 attach 终端的尺寸，并在终端变化后正确渲染。管理器增加了小型的
版本化控制面和 token 校验，避免旧 attach 会话的延迟 resize 请求影响新会话。控制连接丢失
不会中断交互会话，但 resize 可能会短暂延迟。
