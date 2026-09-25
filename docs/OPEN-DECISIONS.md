# 待定决策

Attach 和输出历史设计目前没有待定决策。

之前关于 attach 与历史记录关系的决策已由 ADR 0002 解决：历史使用独立的记录列表和
内容视图。ADR 0003 在实时 attach 输出前增加当前运行的清理快照，但不把 attach 变成
终端回放。ADR 0007 定义 attach 时的崩溃循环诊断和可选持久化日志提示。ADR 0008 定义
独立运行器、管理器接管和 systemd handoff。ADR 0009 定义多用户 systemd 模板、共享文件
和旧安装迁移。ADR 0010 定义事件驱动的 runner 状态、旧 v1 runner 回退和统一
worker supervisor。ADR 0012 定义 macOS LaunchDaemon、共享升级和统一在线安装入口。目前
仍不承诺 runit、s6 或其他 init 集成；需要实际需求后再分别设计对应包。

ADR 0016 将 run 改为默认持久托管，替代 ADR 0011 的临时生命周期。stop 不取消冷启动
恢复，disable 删除注册；存活旧临时服务自动迁移。该功能没有待定决策。

ADR 0014 定义独立配置来源、工作目录和同目录多服务。该功能当前没有待定决策。

ADR 0015 定义 start/stop、手动停止的恢复边界和旧 runner 的能力检测。该功能没有待定决策。
