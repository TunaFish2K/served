# CLI 封装接口

完整版和无头版使用同一套 CLI。无头构建仅裁掉管理菜单和配置表单；保留服务管理、终端 attach、管道 attach、PTY 和日志能力。

```sh
cargo build --release --no-default-features
served version --output json
served list --output json
served history api --list --output json
served history api --output json
served stop api --output json
served attach api --no-stdin | consumer
producer | served attach api --stream
```

## JSON 文档版本 1

全局参数 `--output text|json` 可放在子命令前后，默认 text。
程序参数分隔符 `--` 之后的参数属于子进程，不再由 served 解析。
每次调用向 stdout 写一个 JSON 文档，以换行结束。stderr 留给诊断和 tracing。

```json
{"schema_version":1,"ok":true,"data":{"services":[]}}
{"schema_version":1,"ok":false,"error":{"code":"operation_failed","message":"..."}}
```

成功文档不含 error，失败文档不含 data。调用方先检查退出码和 ok，再读取数据。
首版错误码仅有 `invalid_arguments` 和 `operation_failed`；message 是诊断文本，不是稳定的错误分类。
同一 schema_version 可增加字段；调用方应忽略未知字段。删除字段或改变字段含义需要新的 schema_version。

| 命令 | data |
| --- | --- |
| version | version、variant（full/headless）、features（当前为 `["tui"]` 或 `[]`） |
| list | services 数组 |
| run | name |
| enable/start/stop/restart/disable/shutdown | 空对象 |
| daemon --handoff/--relinquish | 空对象 |
| edit --path、history --path | path |
| history | service、id、current、persisted、raw_bytes、total_lines、content |
| history --list | service、records 数组 |

services 每项包含 name、directory、config_file（可为空）、kind、state、pid（可为空）、tty、restart、persist_logs、attach_active、output_tail。
kind 为 enabled/temporary；state 为 starting/running/restarting/stopped/failed。
records 每项包含 id、bytes、current、persisted。
历史内容经过终端控制序列清理；raw_bytes 是原始字节数，不是清理后内容的长度。

旧 `history --json` 保持原先无封套的格式，与显式 `--output` 互斥。
JSON 模式不启动编辑器，不运行 attach、前台 daemon 或内部 runner；这些组合和缺少子命令均在执行前报错。
`-V`、`--version` 和 `version` 共用版本查询，文字输出为 `served <版本> (full/headless)`，均支持 `--output json`，参数前后顺序不限。
版本选项仅用于顶层查询，与其他子命令混用会报参数错误；`--version version` 合并为一次查询。
`--help` 保持文字输出。

## 管道与非交互调用

stdin 和 stdout 都是终端时，attach 保留 raw mode、独立屏幕、尺寸同步与 Ctrl-C 退出。
任一不是终端，或显式指定 `--stream` / `--no-stdin` 时使用数据流模式。
数据流模式不询问、不清屏、不更新 PTY 尺寸；输入中的 `0x03` 原样转发。
`--no-stdin` 完全不读取输入，适合只观察输出。pipe 服务本身仍是只读观察；只有 PTY 服务接受输入。

stdin EOF 只结束客户端的输入读取，客户端继续等待服务输出；不会把 EOF 传递给服务 stdin。
这适合长驻服务，但不是一次性过滤器：如果服务不结束，attach 会继续等待，调用方需取消连接。
断管、服务结束或信号会解除连接，不停止服务。已有输出先提供清理后的快照，后续实时输出保留服务自身的原始控制字符。
需要纯文本时用 `history --stdout`。attach 不返回服务的退出码。

无参数运行：完整版在终端打开菜单，其他情况显示帮助。
非终端 `history` 默认输出文本；非终端 `edit` 需要 `--path` 或显式 `--editor`。
显式启动的外部编辑器仍可使用其自己的交互和输出行为，不属于 JSON 接口。

## 退出码

- 0：命令成功，包括输出管道被下游正常关闭。启动成功不保证服务一直运行。
- 1：执行失败。
- 2：参数无效或该组合不受支持。
- 130/143：attach 收到 SIGINT/SIGTERM。
- 75：被要求 relinquish 的 manager 进程退出，不是调用该命令的客户端退出码。

显式编辑器的失败状态沿用旧行为。取消调用或输出中途发生 I/O 错误时，不保证有完整 JSON 文档。
EOF、断管及退出行为属于 CLI 合约，不改变 manager/runner 的内部协议。

完整版在交互终端中运行 `served edit` 默认打开配置表单；`$EDITOR` 不覆盖默认表单。
显式 `--editor`、无头版外部编辑器、`--path` 和非交互/JSON 约束保持不变。
表单保存不启用或重启服务。
