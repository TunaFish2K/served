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
- `never`、`on-failure`、`always` 重启策略和指数退避。
- PTY 服务支持 attach、detach，并限制为单个写入客户端。
- 全局 TUI 展示状态、最近输出、restart、disable 和随机 `tips:`。
- 输出只保存在内存 ring buffer 中，不写入持久化日志。
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

## 服务目录

在服务目录中运行 `served edit`，编辑器会创建 `.served.json` 和 `.env` 模板。
最小配置如下：

```json
{
  "name": "api",
  "command": "python app.py",
  "tty": true,
  "restart": "never"
}
```

字段说明：

- `name`：全局唯一的启用服务名，只允许字母、数字、`.`, `_`, `-`。
- `command`：通过 `/bin/sh -c` 执行的命令字符串。
- `tty`：可选，默认 `true`；设置为 `false` 使用管道模式。
- `restart`：可选，默认 `never`；可选值为 `never`、`on-failure`、`always`。

`.env` 只支持配置目录下的这个固定文件。它使用 dotenv 解析规则，可以包含
注释、引号和支持的变量展开，但不会被当作 shell 脚本执行。

manager 启动时记录自己的环境快照；服务启动时再用 `.env` 值覆盖它。修改
`/etc/profile` 等 shell 启动文件不会自动更新已经运行的 manager。

## 命令

```text
served                 打开全局服务 TUI
served daemon          运行 per-user manager
served edit            编辑当前目录的 .served.json 和 .env
served enable          启用当前目录并立即运行
served disable [name]  禁用当前服务，或按名称禁用
served restart [name]  重启当前服务，或按名称重启
served list            列出 manager 管理的服务
```

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

