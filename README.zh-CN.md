# served

[English](README.md)

`served` 是基于 systemd 的轻量部署工具，帮助个人开发者把个人非关键服务部署为长期运行的
服务。它直接管理宿主机进程，由一个 systemd system unit 以安装用户身份启动 manager。

当前代码是 V1 实现，目标平台是 Linux/glibc。

## 适用范围

个人非关键服务是指停止后不会影响主机基础维护能力的服务。机器人、Webhook、个人 API
和 Worker 适合使用 served。`sshd`、登录和网络等基础服务不适合使用 served。

## 部署职责

served 只管理已经存在的项目目录。代码上传、构建和依赖安装由用户负责。served 不负责本地
测试流程。开发者可以直接运行程序进行本地测试，部署后再使用 served 管理常驻服务。

served 不是 Docker 容器运行时，也不提供 root 服务管理、命名空间、资源隔离或健康检查。

## 部署流程

部署需要 Linux/glibc、systemd system manager 和可用的 `sudo`。Release 完整安装包是首选
入口。安装脚本必须由安装用户运行，脚本会在需要时调用 `sudo`。

1. 从 GitHub Release 下载 `full.tar.gz` 安装包。
2. 解压安装包，并进入解压目录。
3. 运行 `./install.sh`。
4. 进入项目目录。
5. 运行 `served edit` 创建并编辑 `.served.json`。
6. 运行 `served enable` 启用项目服务。

安装脚本会安装并启动 `served.service`。这个 unit 只负责启动 served manager。它不会自动
启用项目服务。`served enable` 会把当前项目目录加入管理器，并立即启动该服务。

安装后可以用以下命令确认服务状态：

```bash
served list
served attach <name>
```

项目服务更新后，重新运行 `served restart`。服务异常时，可以使用 `served attach`、
`served history` 和持久化日志排查。served 不会上传项目文件，也不会替用户执行构建流程。

完整安装包包含以下文件，并从包目录运行安装脚本：

```text
served
served.service
install.sh
uninstall.sh
README.md
README.zh-CN.md
```

仓库中的安装脚本位于 `scripts/`，system unit 模板位于 `systemd/`。安装脚本由普通
安装用户执行，并在需要时通过 `sudo` 安装 `/usr/local/bin/served` 和
`/etc/systemd/system/served.service`，然后执行 system scope 的 `daemon-reload`、启用和
启动管理器。Rust 程序不会调用 `systemctl` 或 D-Bus。

安装脚本把可执行文件安装到 `/usr/local/bin/served`。它不会修改用户 shell 配置文件，
安装后无需 PATH export，在任意目录都可以运行 `served`。脚本最后会输出安装路径。

首次安装会直接启用并启动 `served.service` 到 `multi-user.target`。如果
`/usr/local/bin/served` 或 `/etc/systemd/system/served.service` 已存在，脚本会进入
覆盖升级流程。确认覆盖后，文件安装成功会询问是否通过 `systemctl reload` 做 manager
handoff。默认同意时不会停止 runner 或受管服务。handoff 失败会退回受控重启；拒绝时旧
manager 继续运行，并输出稍后手动 reload 的命令。升级失败会尝试恢复旧文件和旧服务。
安装器按 passwd 中的安装用户 home 处理旧 user-service 路径，不依赖被覆盖的 `HOME`
环境变量。

如果升级前服务处于 inactive 或 failed，升级后仍保持停止，但脚本会输出对应的
`systemctl start` 或 `systemctl enable --now` 恢复命令。覆盖升级 handoff 提示回车默认为
同意；卸载提示为 `y/N`，回车取消。卸载确认后会先 disable，再停止运行中的服务，成功后
才删除文件。卸载不会删除配置和状态，也不会修改 shell 配置。非交互环境不会执行需要
确认的操作。

旧版本的 `~/.config/systemd/user/served.service` 或 `~/.local/bin/served` 会被检测。
确认迁移后，脚本会先停止并 disable 旧 user service，确认新的 system service active 后
再删除旧文件。如果旧 user manager 不可用，迁移会安全中止且不会删除旧文件。自定义
XDG 目录只会收到迁移提示，不会被自动复制或删除。

## 命令

```text
served                 打开全局服务 TUI
served daemon          运行 manager；与 system service 使用相同固定路径
served edit            用外部编辑器打开当前目录的 .served.json
served edit -e <cmd>   指定编辑器命令，优先于 $EDITOR
served edit --path     创建缺失模板后只打印 .served.json 路径
served enable          启用当前目录并立即运行
served disable [name]  禁用当前服务，或按名称禁用
served restart [name]  重启当前服务，或按名称重启
served attach [name]   直接 attach 当前服务，或按名称 attach
served history [name]  用 $EDITOR 打开 latest.log
served history [name] --run <id>
                       用编辑器打开指定归档，例如 20260724-233045.log
served history [name] -e <command>
                       指定编辑器命令，优先于 $EDITOR
served history [name] --path
                       只打印选中持久化日志的路径
served list            列出 manager 管理的服务
```

安装的 systemd 服务使用 `systemctl reload served` 做 manager handoff。升级 manager 时
不会重启受管服务。`systemctl restart served` 和 `systemctl stop served` 是明确的服务
生命周期操作，会停止 runner。manager 异常退出或被 systemd 自动拉起时，runner 会继续
运行，并在新 manager 启动后被接管。

`served attach` 不会启动服务管理 TUI。省略名称时使用当前目录对应的已启用服务；提供
名称时可以从任意目录 attach。目标服务必须正在运行。attach 会话进入终端第二屏，先显示
当前运行最近 48 个逻辑行的清理快照，再接入实时输出。快照最多约 16 KiB，不会发送给
服务，也不是 PTY 屏幕状态回放。退出时恢复原来的 shell 或 TUI 画面。

`tty: true` 服务的会话可写入 PTY；`tty: false` 服务只转发快照和实时 stdout/stderr，
并忽略输入。pipe 服务可以有多个只读观察者。attach 会话中按 `Ctrl-C` 退出 attach，
服务本身不会被停止；该按键不会转发给服务。

对于 `tty: true`，attach 首次连接和终端尺寸变化会更新服务 PTY 的 rows/cols；
`syncRowsCols: false` 时保持 PTY 的初始尺寸。控制连接断开时原始 attach 仍可继续，
客户端会在后台重连并重新发送当前尺寸；detach 不会把尺寸重置为初始值。

runner 会记录最近 60 秒内的非零退出和 worker 启动或运行错误。失败达到 3 次后，如果
用户在服务重启退避期间尝试 attach，CLI 和 TUI 会显示崩溃循环警告。直接 CLI attach
只在交互终端中询问 `Open latest.log? [y/N]`；TUI 使用 `y` 或 `Enter` 打开，`n` 或
`Esc` 取消。只有当前运行启用了 `persist_logs` 时才有可打开的 `latest.log`，否则提示
使用 TUI 历史浏览器或启用持久化。打开日志使用 `$EDITOR`，退出编辑器后 attach 仍返回
原来的“服务未运行”错误，不会自动重试。该警告只在 attach 失败时出现，不改变服务列表
状态。

按 `h` 后先选择 `latest` 或时间归档，再按 `Enter` 查看清理后的日志内容。内容页支持
上下键、`j/k`、`PgUp/PgDn` 和 `g/G`，并在内容与 `tips:` 之间显示当前逻辑行位置
`current/total`。总行数按清理后的 `str::lines()` 计算，视觉换行不会改变总数。历史页
与 attach 分离，attach 不会回放旧的 PTY 控制状态。

命令行 `served history` 不直接输出日志内容，而是打开选中的持久化原始日志文件。没有
`--run` 时选择 `latest`；`-e/--editor COMMAND` 优先于 `$EDITOR`，编辑器命令可包含
参数，日志路径会作为最后一个安全引用的参数传入。`--path` 只打印路径，并且与
`--editor` 互斥。非持久化记录只有 TUI 内存副本，没有文件路径；命令行会提示使用 TUI
或启用 `persist_logs`。

日志历史按每次进程启动分段，包括首次启动、自动重启和手动重启。日志由每个服务的
runner 持有；manager 重启不会丢失 runner 内的历史。持久化日志位于：

```text
$HOME/.local/state/served/logs/<name>/
```

当前运行写入 `latest.log`。下一次启动时按旧运行的开始时间归档为
`YYYYMMDD-HHMMSS.log`；同一秒冲突时追加 `-1`、`-2`。`.latest.started` 保存当前记录的
开始时间。每个服务最多保留 100 个归档和一个 latest，日志目录权限为 `0700`，日志文件
权限为 `0600`。

`persist_logs: false` 时不会新增磁盘日志。runner 运行期间保留当前记录和最近 100 条
内存归档；manager 重启后这些记录仍可查看，runner 或服务真正重启后才开始新的当前记录。

TTY 日志保存原始 PTY 字节；pipe 日志按 runner 收到的顺序合并 stdout/stderr。历史展示
会移除 ANSI 和不可见控制序列。持久化写入失败时服务继续运行，并降级到内存，同时记录
manager warning。

V1 不提供面向服务的独立 `start`、`stop` 或 `reload` 命令。修改配置后使用 `restart`；
manager 会先完整校验新配置，校验失败时保留旧进程不变。启用服务后，注册链接位于：

```text
~/.config/served/enabled/<name> -> /path/to/service-directory
```

## 服务目录

在服务目录中运行 `served edit`。如果 `.served.json` 不存在，命令会先创建带详细注释的
JSON5 模板，再用外部编辑器打开。已有文件不会被重写或格式化。最小配置如下：

```json5
{
  name: "api",
  command: "python app.py",
  tty: true,
  syncRowsCols: true,
  restart: "never",
  persist_logs: false,
  env: {
    // PORT: "8080",
  },
}
```

JSON5 支持注释、单引号或双引号、不加引号的字段名和尾逗号。模板会解释每个字段的作用，
不需要另外查文档。字段说明如下：

- `name`：全局唯一的启用服务名，只允许字母、数字、`.`, `_`, `-`。
- `command`：通过 `/bin/sh -c` 执行的命令字符串。
- `command` 可以是多行 shell 脚本。JSON5 字符串中的 `\n` 表示实际换行；如果参数需要
  字面量 `\n`，应写成 `\\n`。
- `tty`：可选，默认 `true`。设置为 `false` 后使用管道模式。
- `syncRowsCols`：可选，默认 `true`。attach TTY 服务时把 PTY 尺寸同步为当前终端尺寸。
  `tty: false` 时该字段保留但不生效。
- `restart`：可选，默认 `never`。可选值为 `never`、`on-failure`、`always`。
- `persist_logs`：可选，默认 `false`。设置为 `true` 后，将每次运行的完整输出保存到
  `$HOME/.local/state/served/logs/<name>/`。配置修改在下次启动或重启时生效。
- `env`：可选对象，值都是字面量字符串，不会做 shell 或变量展开。它的优先级高于
  manager 环境和旧 `.env.served` 中的同名键。

`.env.served` 只支持配置目录下的这个固定文件，且不会由新的 `served edit` 创建或编辑。
如果它已经存在，served 仍会先按 dotenv 规则读取它，以兼容旧配置；JSON5 中的 `env`
随后覆盖同名键。

manager 启动时记录自己的环境快照。服务启动时按 manager 环境、旧 `.env.served`、JSON5
`env` 的顺序叠加。修改 `/etc/profile` 等 shell 启动文件不会自动更新已经运行的 manager。

## TUI 操作

全局 TUI 底部同时显示随机 `tips:` 和上下文操作栏。没有选中服务时，操作栏显示
`up/down/j/k move` 与退出。选中服务后会显示 `r restart`、`d disable`、`a attach`
和 `h history`。TTY 服务的 attach 可向服务写入；`tty: false` 服务的 attach 只读；两者
都进入终端第二屏。窄终端会把操作栏自动换成两行。

主 TUI 不再编辑服务配置。`served edit` 会直接把 `.served.json` 交给外部编辑器：
`-e/--editor COMMAND` 优先使用指定命令，否则使用 `$EDITOR`。命令可以带参数，配置
路径会作为最后一个参数传入。`--path` 会先创建缺失模板，然后只打印配置的绝对路径，
并且与 `--editor` 互斥。没有可用编辑器时，命令会给出明确错误。

## Tag 发布

推送与 `Cargo.toml` 版本一致的 `v<semver>` tag 会自动创建 GitHub Release，并构建
Linux amd64/glibc 产物。例如版本 `0.1.8` 使用 tag `v0.1.8`，Release 会包含：

```text
served-linux-amd64-v0.1.8-binary
served-linux-amd64-v0.1.8-binary.sha256
served-linux-amd64-v0.1.8-full.tar.gz
served-linux-amd64-v0.1.8-full.tar.gz.sha256
```

`binary` 只包含可执行文件；`full.tar.gz` 是完整安装包，包含 `served`、
`served.service`、`install.sh`、`uninstall.sh` 和两个 README 文件。每个产物都有自己的
SHA-256 sidecar 文件，文件名是在原文件名后追加 `.sha256`。

当前 workflow 只构建 Linux amd64/glibc，不构建 ARM、musl 或其他操作系统。

## 安全边界

- manager 以普通用户身份运行，socket 设置为用户可读写。
- systemd system unit 以安装用户身份启动和监督 manager；每个服务由独立 runner
  持有，manager 通过私有 runner socket 接管它。
- manager 或其 system unit 重启后，enabled registry 会被重新扫描，并优先接管已有 runner。
- runner 位于 `$HOME/.local/state/served/runtime/runners/<name>/`，持有服务进程、PTY、
  日志缓存、自动重启状态和 crash-loop 窗口。manager 异常退出不会停止它们。
- `systemctl stop served` 通过 graceful shutdown 停止所有 runner；`served disable` 和
  `served restart` 也会停止或替换对应 runner。升级使用 `systemctl reload served` 做
  manager handoff，保留服务 PID；首次从旧 worker 架构升级时可能需要一次受控重启。
- system service 按安装用户的登录环境设置 `HOME`，并通过 login shell 启动 manager，
  因此 `/etc/profile` 等环境会在 manager 启动时被读取。manager 运行期间仍使用启动时的
  环境快照。
- system service 的工作目录使用安装用户的 home，不依赖 system manager 的 `%h` 展开。
- 停止服务先发送 `SIGTERM`，超时后发送 `SIGKILL`。manager 异常退出不会执行这条清理
  路径；runner 只在明确 shutdown、disable 或 restart 时终结受管 shell。
- `nohup`、后台化、daemonize 等脱离受管 shell 的子进程不在清理保证内。
- V1 不提供 root 模式、容器隔离、namespace、资源限制、依赖图或健康检查。

## 维护者构建

以下命令用于构建和检查 served 本身。部署个人项目时，不需要先安装 Rust 工具链。个人部署
应优先使用 Release 完整安装包。

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

核心需求记录在 [REQUIREMENTS.md](REQUIREMENTS.md)，技术决策记录在
[TECH-STACK.md](TECH-STACK.md)。

## 许可证

served 使用 [Unlicense](LICENSE) 发布。你可以自由使用、复制、修改、发布和分发本项目。
本软件不提供任何形式的保证。
