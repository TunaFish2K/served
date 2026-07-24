# served

`served` 是一个面向 Linux 本地开发环境的轻量级、按用户运行的服务管理器。
它直接管理宿主机进程，用一个 `systemd --user` unit 负责启动 manager；它不是
Docker 替代的容器运行时，也不负责 root 服务、命名空间或资源隔离。

当前代码是 V1 implementation baseline，目标平台是 Linux/glibc。

## 特性

- 一个服务对应一个目录和一个 `.served.json`。
- 服务工作目录固定为配置目录。
- `.env` 只读取当前服务目录中的文件，并覆盖 manager 的启动环境。
- 默认使用 PTY，可用 `tty: false` 改用 stdout/stderr 管道。
- TTY attach 默认把服务 PTY 的 rows/cols 同步为当前 attach 终端尺寸，可用
  `syncRowsCols: false` 关闭。
- `never`、`on-failure`、`always` 重启策略和指数退避。
- PTY 服务支持可写 attach；pipe 服务支持只读 attach。所有 attach 都使用终端第二屏。
- `served attach [name]` 可绕过 TUI 直接进入 attach 会话。
- 全局 TUI 展示状态、restart、disable、attach、历史和随机 `tips:`。
- 日志可以按服务选择持久化，也可以只保存在 manager 内存中。
- manager 与 CLI/TUI 通过 `$XDG_RUNTIME_DIR` 下的用户 Unix socket 通信。

## 快速开始

需要 Rust stable、Linux 和可用的用户级 systemd。

```bash
cargo build --release
cargo test
```

manager 通常由 `systemd --user` 启动。离线发布包应包含以下文件，并从包目录
运行安装脚本：

```text
served
served.service
install.sh
uninstall.sh
```

仓库中的安装脚本位于 `scripts/`，user unit 模板位于 `systemd/`。安装脚本负责
安装二进制、user unit、`daemon-reload`、启用服务和设置 linger；Rust 程序不会
调用 `systemctl`、`loginctl` 或 D-Bus。

安装脚本把可执行文件放到 `~/.local/bin`，不会修改用户的 shell 配置文件。安装
成功后会输出以下命令，复制到当前 shell 执行即可在当前会话的任意目录运行
`served`：

```bash
export PATH="$HOME/.local/bin:$PATH"
```

如果希望新开的 shell 也能直接运行 `served`，请自行把这条命令加入你的 shell
配置文件。卸载脚本不会修改用户 shell 配置。

首次安装会直接启用并启动 `served.service`。如果 `~/.local/bin/served` 或
`~/.config/systemd/user/served.service` 已存在，脚本会进入覆盖升级流程：确认覆盖
后，只有运行中的服务才会再次询问是否停止；文件安装成功后，原本运行中的服务
会询问是否重启，原本停止的服务保持停止。升级失败会尝试恢复旧文件和旧服务。
覆盖升级、停止和重启提示回车默认为同意；卸载提示为 `y/N`，回车取消。卸载
确认后会先 disable，再停止运行中的服务，成功后才删除文件；卸载不会修改 shell
配置。非交互环境不会执行需要确认的操作。

## Tag 发布

推送与 `Cargo.toml` 版本一致的 `v<semver>` tag 会自动创建 GitHub Release，并
构建 Linux amd64/glibc 产物。例如版本 `1.0.5` 使用 tag `v1.0.5`，Release 会
包含：

```text
served-linux-amd64-v1.0.5-binary
served-linux-amd64-v1.0.5-binary.sha256
served-linux-amd64-v1.0.5-full.tar.gz
served-linux-amd64-v1.0.5-full.tar.gz.sha256
```

`binary` 是只包含可执行文件的产物；`full.tar.gz` 是包含 `served`、
`served.service`、`install.sh`、`uninstall.sh` 和本 README 的完整安装包。每个
产物都有自己的 SHA-256 sidecar 文件，文件名是在原文件名后追加 `.sha256`。

当前 workflow 只构建 Linux amd64/glibc，不构建 ARM、musl 或其他操作系统。

## 服务目录

在服务目录中运行 `served edit`，编辑器会创建 `.served.json` 和 `.env` 模板。
最小配置如下：

```json
{
  "name": "api",
  "command": "python app.py",
  "tty": true,
  "syncRowsCols": true,
  "restart": "never",
  "persist_logs": false
}
```

字段说明：

- `name`：全局唯一的启用服务名，只允许字母、数字、`.`, `_`, `-`。
- `command`：通过 `/bin/sh -c` 执行的命令字符串。
- `command` 可以是多行 shell 脚本；编辑器会逐行显示实际换行。JSON 中的 `\n`
  表示实际换行，保存后仍由 shell 按多行脚本执行；如果参数需要字面量 `\n`，
  JSON 中应写成 `\\n`。
- `tty`：可选，默认 `true`；设置为 `false` 使用管道模式。
- `syncRowsCols`：可选，默认 `true`；attach TTY 服务时把 PTY 尺寸同步为当前终端
  尺寸。`tty: false` 时该字段保留但不生效。
- `restart`：可选，默认 `never`；可选值为 `never`、`on-failure`、`always`。
- `persist_logs`：可选，默认 `false`；设置为 `true` 将每次运行的完整输出保存到
  XDG state 日志目录。配置修改在下次启动或重启时生效。

`.env` 只支持配置目录下的这个固定文件。它使用 dotenv 解析规则，可以包含
注释、引号和支持的变量展开，但不会被当作 shell 脚本执行。

manager 启动时记录自己的环境快照；服务启动时再用 `.env` 值覆盖它。修改
`/etc/profile` 等 shell 启动文件不会自动更新已经运行的 manager。

## TUI 操作

全局 TUI 底部会同时显示随机 `tips:` 和上下文操作栏。没有服务时，操作栏显示
`up/down/j/k move` 与退出；选中服务后会显示 `r restart`、`d disable`、`a attach`
和 `h history`。TTY 服务的 attach 可写入服务，`tty: false` 服务的 attach 只读；两者
都进入终端第二屏。操作栏在窄终端中自动换行到两行。

`served edit` 的字段从上到下依次是 `name`、`command`、`TTY`、`sync rows/cols`、
`restart`、`persist logs` 和 `.env`。command 区域会按行数动态扩展，标题显示总行数，过长时
支持滚动；粘贴文本会保留多行结构，CRLF 和 CR 会规范为 LF。使用 `Tab` 前进、`Shift-Tab` 后退；TTY、`sync rows/cols` 和 `persist logs`
字段显示 `Enabled/Disabled`，restart 字段显示 `never/on-failure/always`。焦点在
选择字段时按 `Enter`
打开菜单，用上下方向键或 `j/k` 移动，`Enter` 应用选择，`Esc` 关闭菜单且不应用
暂存值。普通编辑状态下底部会显示当前可用按键；`Ctrl-S` 保存，`Esc` 或
`Ctrl-C` 取消整个编辑。编辑器也会在启动时显示一条随机 `tips:`。

## 命令

```text
served                 打开全局服务 TUI
served daemon          运行 per-user manager
served edit            编辑当前目录的 .served.json 和 .env
served enable          启用当前目录并立即运行
served disable [name]  禁用当前服务，或按名称禁用
served restart [name]  重启当前服务，或按名称重启
served attach [name]   直接 attach 当前服务，或按名称 attach
served history [name]  列出服务的 latest 和时间归档
served history [name] --run <id>
                       输出指定历史记录，例如 latest 或 20260724-233045.log
served list            列出 manager 管理的服务
```

`served attach` 不启动服务管理 TUI。省略名称时使用当前目录对应的已启用服务；
提供名称时可以从任意目录 attach。目标服务必须正在运行。attach 会话进入终端第二屏，
先显示当前运行最近 48 个逻辑行的清理快照，再接入实时输出；快照最多约 16 KiB，
不会发送给服务，也不是 PTY 屏幕状态回放。退出时恢复原来的 shell 或 TUI 画面。
`tty: true` 服务的会话可写入 PTY；`tty: false` 服务只转发快照和实时 stdout/stderr，
并忽略输入。pipe 服务可以有多个只读观察者。attach 会话中按 `Ctrl-C` 退出 attach，
服务本身不会被停止；该按键不会转发给服务。
对于 `tty: true`，attach 首次连接和终端尺寸变化会更新服务 PTY 的 rows/cols；
`syncRowsCols: false` 时保持 PTY 的初始尺寸。控制连接断开时原始 attach 仍可继续，
客户端会在后台重连并重新发送当前尺寸；detach 不会把尺寸重置为初始值。

按 `h` 后先选择 `latest` 或时间归档，再按 `Enter` 查看清理后的日志内容；内容页
支持上下键、`j/k`、`PgUp/PgDn` 和 `g/G`。历史页与 attach 分离，attach 不会回放
旧的 PTY 控制状态。

日志历史按每次进程启动分段，包括首次启动、自动重启和手动重启。持久化日志位于：

```text
$XDG_STATE_HOME/served/logs/<name>/
~/.local/state/served/logs/<name>/  # 未设置 XDG_STATE_HOME 时
```

当前运行写入 `latest.log`，下一次启动时按旧运行的开始时间归档为
`YYYYMMDD-HHMMSS.log`；同一秒冲突时追加 `-1`、`-2`。`.latest.started` 保存当前
记录的开始时间。每个服务最多保留 100 个归档和一个 latest，日志目录权限为 `0700`，
日志文件权限为 `0600`。`persist_logs: false` 时不会新增磁盘日志，manager 运行期间
保留当前记录和最近 100 条内存归档；manager 重启后内存历史清空，已有磁盘归档仍可查看。

TTY 日志保存原始 PTY 字节，pipe 日志按 manager 收到的顺序合并 stdout/stderr；历史
展示会移除 ANSI 和不可见控制序列。持久化写入失败时服务继续运行并降级到内存，同时
记录 manager warning。

V1 不提供独立的 `start`、`stop` 或 `reload` 命令。修改配置后使用 `restart`；
manager 会先完整校验新配置，校验失败时保留旧进程不变。启用服务后，注册链接
位于：

```text
~/.config/served/enabled/<name> -> /path/to/service-directory
```

## 安全边界

- manager 以普通用户身份运行，socket 设置为用户可读写。
- `systemd --user` 只负责启动和监督 manager；manager 直接管理服务子进程。
- manager 或其 user unit 重启后，enabled registry 会被重新扫描。
- 停止服务先发送 `SIGTERM`，超时后发送 `SIGKILL`。
- `nohup`、后台化、daemonize 等脱离受管 shell 的子进程不在清理保证内。
- V1 不提供 root 模式、容器隔离、namespace、资源限制、依赖图或健康检查。

## 开发

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

核心需求记录在 [REQUIREMENTS.md](REQUIREMENTS.md)，技术决策记录在
[TECH-STACK.md](TECH-STACK.md)。
