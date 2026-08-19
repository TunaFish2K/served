# 待定决策

Attach 和输出历史设计目前没有待定决策。

之前关于 attach 与历史记录关系的决策已由 ADR 0002 解决：历史使用独立的记录列表和
内容视图。ADR 0003 在实时 attach 输出前增加当前运行的清理快照，但不把 attach 变成
终端回放。ADR 0007 定义 attach 时的崩溃循环诊断和可选持久化日志提示。ADR 0008 定义
独立运行器、管理器接管和 systemd handoff。ADR 0009 定义多用户 systemd 模板、共享文件
和旧安装迁移。ADR 0010 定义事件驱动的 runner 状态、旧 v1 runner 回退和统一
worker supervisor。目前仍不承诺 launchd、runit、s6 或其他 init 集成；需要实际需求后再
分别设计对应包。
