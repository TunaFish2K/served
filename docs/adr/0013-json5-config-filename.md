# ADR 0013：JSON5 配置文件名与旧后缀兼容

- 状态：已接受
- 日期：2026-08-28
- 部分取代：ADR 0006 的固定 `.served.json` 文件名决策

## 背景

服务配置已经按 JSON5 解析，但 `.served.json` 后缀仍表示严格 JSON，容易误导编辑器、格式化
工具和用户。新文件名应准确表达格式，同时不能中断已有服务目录。

## 决策

- 新服务配置文件名是 `.served.json5`。`served edit` 只在两种文件都不存在时创建该文件。
- `.served.json` 是弃用的兼容输入，仍按 JSON5 解析。served 不自动复制、重命名或删除它，
  也不设定移除版本。
- 只有旧文件时，manager 和 `served edit` 使用它并输出弃用 warning。
- 两个文件同时存在时，`.served.json5` 优先，并输出旧文件被忽略的 warning。新文件无效时
  直接报错，不回退到旧文件。
- manager 把 warning 写入 tracing 日志。`served edit` 把 warning 写入 stderr，保持
  `served edit --path` 的 stdout 只有配置路径。
- 临时服务忽略两种配置文件。配置 schema、JSON5 解析、环境优先级和 IPC 协议不变。

## 结果

新项目和编辑器能从后缀识别 JSON5。已有服务继续运行，并在正常管理操作中收到可执行的迁移
提示。明确的优先级避免两个文件产生不稳定或静默回退行为。
