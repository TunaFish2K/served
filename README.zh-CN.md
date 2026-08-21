# served

[English](README.md)

`served` 帮助个人开发者把个人非关键服务部署为长期运行的服务。它直接管理宿主机进程，
不运行容器。前台 manager 可以交给任意进程守护程序托管；仓库提供可选的 systemd 和
launchd 集成。

Release 支持 macOS 和 Linux/glibc，并提供 amd64/x64 和 arm64 二进制。

## 适用范围

个人非关键服务是指停止后不会影响主机基础维护能力的服务。机器人、Webhook、个人 API
和 Worker 适合使用 served。`sshd`、登录和网络等基础服务不适合使用 served。

## 部署职责

served 只管理已经存在的项目目录。代码上传、构建和依赖安装由用户负责。served 不负责本地
测试流程。开发者可以直接运行程序进行本地测试，部署后再使用 served 管理常驻服务。

served 不是 Docker 容器运行时，也不提供 root 服务管理、命名空间、资源隔离或健康检查。

## 部署流程

systemd Linux 主机和 macOS 主机可以用一条命令安装最新稳定版及对应 supervisor 集成：

```bash
curl -fsSL https://raw.githubusercontent.com/TunaFish2K/served/main/scripts/install-online.sh | sh
```

以后重新运行同一命令即可升级。脚本检测系统和架构，下载 full 包及 SHA-256 sidecar，校验
后才执行包内安装器。安装需要可用的 `sudo`。使用其他 supervisor 时，可以手动下载 binary
资产，并以目标用户身份和该用户正常的 `HOME` 运行以下前台命令：

```bash
served daemon
```

优雅停止使用 `served shutdown`。替换二进制后，使用 `served daemon --handoff` 切换 manager，
同时保留 runner 和受管服务。向前台 manager 发送 `SIGTERM` 或 `SIGINT` 也会执行优雅停止。

Linux full 包会启用 `served@$USER.service`。macOS full 包会安装名为
`io.github.tunafish2k.served.<uid>` 的 system LaunchDaemon。两种安装器都不会自动启用项目
服务。

manager 启动后：

1. 进入项目目录。
2. 运行 `served edit` 创建并编辑 `.served.json`。
3. 运行 `served enable` 启用并启动项目服务。

如需运行不使用项目配置的临时服务，请使用 `served run`：

```bash
served run -- python app.py
```

安装后可以用以下命令确认服务状态：

```bash
served list
served attach <name>
```

项目服务更新后，重新运行 `served restart`。服务异常时，可以使用 `served attach`、
`served history` 和持久化日志排查。served 不会上传项目文件，也不会替用户执行构建流程。

Linux 完整安装包包含 `served`、`served@.service`、`install.sh`、`uninstall.sh`、README 和
许可证。macOS 完整包用 `served.plist` 代替 systemd 模板。

以下 systemd 安装流程只适用于 Linux。仓库中的安装脚本位于 `scripts/`，system unit 模板
位于 `systemd/`。脚本由拥有 manager 的普通用户执行，通过 `sudo` 安装共享的
`/usr/local/bin/served` 和 `/etc/systemd/system/served@.service`，然后启用并启动
`served@$USER.service`。Rust 程序不会调用 `systemctl` 或 D-Bus。

模板使用 `User=%i`。每个实例分别使用该账户的登录环境、home、socket、registry、runner
和受管服务。模板拒绝 `root` 实例，并且不设置 `Group=`，因此 systemd 使用账户的主组。
共享文件安装后，可以显式增加其他账户：

```bash
sudo systemctl enable --now served@alice.service
```

主机首次安装时，会在 `multi-user.target` 启用并启动调用账户的实例。升级会保留每个实例的
enabled 和 active 状态。共享二进制变化后，安装器 reload 所有活动的
`served@*.service`；新客户端会把新可执行文件的绝对路径交给 manager，因此目标路径被
替换后也能 handoff。handoff 失败时只对该实例受控重启；停止的实例保持停止。文件或服务
操作失败时，安装器会恢复旧共享文件，并尝试恢复记录的实例状态。

安装器会自动检测旧的固定 `/etc/systemd/system/served.service`、
`~/.config/systemd/user/served.service` 和 `~/.local/bin/served`，并确认固定 unit 属于
当前账户。固定服务活动时，安装器先升级 manager，再让它释放 socket 但保留 runner，最后
启动模板实例接管。无法完成转移时才执行受控停止。只有新实例达到目标状态后才删除旧文件。
自定义 XDG 目录只报告提示，不自动移动。

以目标账户运行 `./uninstall.sh` 只会 disable 和停止该账户实例，并保留配置和状态。如果
还有其他 enabled 或 active 实例，脚本会保留共享二进制和模板；否则使用单独的 `y/N`
提示决定是否删除共享文件。需要确认的操作在非交互环境中不会执行。

## 可选 launchd 安装

macOS full 包把共享二进制安装到 `/usr/local/bin/served`，并为当前用户安装
`/Library/LaunchDaemons/io.github.tunafish2k.served.<uid>.plist`。plist 由 root 拥有，但通过
`UserName` 让 manager 和所有服务以安装用户身份运行。它使用该账户的 home、工作目录、登录
shell、主组、socket、registry、runner 和日志，不依赖图形登录会话。

首次安装会 bootstrap 并启动当前用户实例。升级替换共享二进制后，会 handoff 主机上所有
活动 served LaunchDaemon。当前用户 plist 发生变化时，安装器先让 manager relinquish，再
重新 bootstrap，使新 manager 接管已有 runner。任何失败都会恢复旧二进制、plist 和活动
manager。原本未加载的实例保持未加载。

以目标账户运行包内 `./uninstall.sh`，只会停止并删除该用户 LaunchDaemon，配置和状态始终
保留。仍有其他 served LaunchDaemon 时保留共享二进制；否则用单独的 `y/N` 提示决定是否删除。

macOS 隐私控制可能阻止 LaunchDaemon 访问受保护的 Desktop、Documents 或 Downloads 目录。
可以为 `/usr/local/bin/served` 授予 Full Disk Access，或把项目放在不受保护的目录。Release
使用 ad-hoc 签名，不做 notarization。

## 命令

```text
served                 打开全局服务 TUI
served daemon          用固定 HOME 路径运行前台 manager
served daemon --handoff
                       替换 manager，同时保留 runner
served daemon --relinquish
                       退出 manager，但保留 runner 供另一个守护程序接管
served shutdown        停止 manager 和所有受管 runner
served edit            用外部编辑器打开当前目录的 .served.json
served edit -e <cmd>   指定编辑器命令，优先于 $EDITOR
served edit --path     创建缺失模板后只打印 .served.json 路径
served enable          启用当前目录并立即运行
served run [选项] -- <程序> [参数...]
                       不使用项目配置创建临时服务
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
served history [name] --stdout
                       输出清理后的持久化或内存历史
served history [name] --json
                       用 JSON 输出清理后的历史和元数据
served list            列出 manager 管理的服务
```

`served run` 在当前目录创建临时服务。manager 必须已经运行。该命令不读取或创建
`.served.json` 和 `.env.served`，也不创建启用链接。创建成功后，该命令输出服务名并退出。

```bash
served run --name api --no-tty --restart on-failure \
  --env PORT=8080 -- python app.py --verbose
```

服务名默认使用清洗后的当前目录名。served 默认分配 TTY。served 默认根据 attach 客户端
同步 PTY 尺寸。默认重启策略是 `never`。日志默认只保存在内存中。`--restart` 接受
`never`、`on-failure` 或 `always`。

`--no-tty` 和 `--no-sync-rows-cols` 可以关闭对应的 TTY 选项。`--persist-logs` 把 raw 日志
写入磁盘。`--log-max-bytes` 和 `--log-max-files` 的默认值与 `.served.json` 相同。

每个 `--env KEY=VALUE` 都会覆盖 manager 环境快照中的同名键。一个键重复出现时，最后一个
值生效。

服务名在所有受管服务中必须唯一。一个目录只能有一个受管服务。发生名称或目录冲突时，
`served run` 会返回错误。该命令不会修改已有服务。

`--` 后的参数保持原始边界。served 不解释这些参数中的 shell 语法。命令需要管道、重定向
或变量展开时，请显式使用 `sh -c`。

TUI 和 `served list` 都会显示临时服务。临时服务也支持 attach、history、restart 和
disable。程序退出后，服务保持 `stopped` 状态。此时仍可查看历史或重启服务。
`served disable` 删除私有 runtime 描述。它不会删除持久化日志。

manager handoff、relinquish 和异常崩溃都会保留 runner。新的 manager 可以接管仍在运行的
临时服务。manager 使用私有 runtime 描述验证 runner。shutdown 和正常停止 manager 会删除
该描述。主机重启后，manager 不会恢复该服务。

安装的 systemd 服务使用 `systemctl reload "served@$USER.service"` 做 manager handoff。
`systemctl restart` 和 `systemctl stop` 是该账户的明确生命周期操作，会停止其 runner。
manager 异常退出后，systemd 会启动新 manager 并接管仍在运行的 runner。

`served attach` 不会启动服务管理 TUI。省略名称时使用当前目录对应的受管服务；提供
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
使用 TUI 历史浏览器或 `served history --stdout`。打开日志优先使用 `$EDITOR`；未配置时按
`editor`、`sensible-editor`、`nvim`、`vim`、`vi`、`nano`、`micro`、`hx` 的顺序查找。
退出编辑器后 attach 仍返回原来的“服务未运行”错误，不会自动重试。该警告只在 attach
失败时出现，不改变服务列表状态。

按 `h` 后先选择 `latest` 或时间归档，再按 `Enter` 查看清理后的日志内容。内容页支持
上下键、`j/k`、`PgUp/PgDn` 和 `g/G`，并在内容与 `tips:` 之间显示当前逻辑行位置
`current/total`。总行数按清理后的 `str::lines()` 计算，视觉换行不会改变总数。历史页
与 attach 分离，attach 不会回放旧的 PTY 控制状态。

命令行 `served history` 没有 `--run` 时选择 `latest`。没有输出参数时，它打开选中的持久化
原始日志；`-e/--editor COMMAND` 优先于 `$EDITOR` 和自动探测结果。`--path` 只打印持久化
路径。`--stdout` 输出清理后的内容，`--json` 输出内容以及服务、记录、存储、字节和行数
元数据；两者都支持持久化与内存记录，也不会创建临时文件。`--path`、`--editor`、
`--stdout`、`--json` 是互斥的输出模式。

日志历史按每次进程启动分段，包括首次启动、自动重启和手动重启。日志由每个服务的
runner 持有；manager 重启不会丢失 runner 内的历史。持久化日志位于：

```text
$HOME/.local/state/served/logs/<name>/
```

当前运行写入 `latest.log`。达到 `log_max_bytes` 后，served 按运行开始时间归档当前段为
`YYYYMMDD-HHMMSS.log`，并继续写入新的 `latest.log`；同一秒冲突时追加 `-1`、`-2`。
`.latest.started` 保存当前记录的
开始时间。每个服务保留 `log_max_files` 个归档和一个 latest。默认值是每段 `10 MiB`、
保留 `3` 个归档。日志目录权限为 `0700`，日志文件权限为 `0600`。

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
  log_max_bytes: 10485760,
  log_max_files: 3,
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
- `log_max_bytes`：可选，默认 `10485760` 字节（`10 MiB`）。持久化日志段达到这个大小后，
  served 会归档当前段，并继续写入新的 `latest.log`。
- `log_max_files`：可选，默认 `3`。表示保留的持久化归档段数量。`latest.log` 不计入这个数量。
  服务启动或日志轮转时会删除过旧或超过单文件上限的归档。
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
`-e/--editor COMMAND` 优先使用指定命令，其次使用 `$EDITOR`，最后按 `editor`、
`sensible-editor`、`nvim`、`vim`、`vi`、`nano`、`micro`、`hx` 的顺序查找可执行文件。
命令可以带参数，配置路径会作为最后一个参数传入。`--path` 会先创建缺失模板，然后只打印
配置的绝对路径，并且与 `--editor` 互斥。没有可用编辑器时，命令会给出明确错误。

## Tag 发布

推送与 `Cargo.toml` 版本一致的 `v<semver>` tag 会自动创建 GitHub Release，并构建 macOS
和 Linux 的 amd64、arm64 产物。Linux 二进制最低需要 glibc 2.17；macOS amd64 最低支持
10.12，arm64 最低支持 11.0。发布 tag 为 `vX.Y.Z` 时，产物使用以下命名格式：

```text
served-linux-amd64-vX.Y.Z-binary
served-linux-amd64-vX.Y.Z-binary.sha256
served-linux-amd64-vX.Y.Z-full.tar.gz
served-linux-amd64-vX.Y.Z-full.tar.gz.sha256
served-linux-arm64-vX.Y.Z-binary
served-linux-arm64-vX.Y.Z-binary.sha256
served-linux-arm64-vX.Y.Z-full.tar.gz
served-linux-arm64-vX.Y.Z-full.tar.gz.sha256
served-macos-amd64-vX.Y.Z-binary
served-macos-amd64-vX.Y.Z-binary.sha256
served-macos-amd64-vX.Y.Z-full.tar.gz
served-macos-amd64-vX.Y.Z-full.tar.gz.sha256
served-macos-arm64-vX.Y.Z-binary
served-macos-arm64-vX.Y.Z-binary.sha256
served-macos-arm64-vX.Y.Z-full.tar.gz
served-macos-arm64-vX.Y.Z-full.tar.gz.sha256
served-vX.Y.Z-source.tar.gz
served-vX.Y.Z-source.tar.gz.sha256
```

每个平台的 `binary` 只包含可执行文件。Linux full 包增加 systemd 集成，macOS full 包增加
LaunchDaemon 集成。可重复生成的 source 压缩包包含可构建的项目源码。每个产物都有自己的
SHA-256 sidecar。macOS 二进制使用 ad-hoc 签名，不做 notarization。当前 workflow 不构建
musl 或 Windows 目标。

## 安全边界

- manager 以普通用户身份运行，socket 设置为用户可读写。
- 进程守护程序以安装用户身份启动前台 manager；systemd unit 和 macOS LaunchDaemon 是
  受支持的平台集成。每个服务由独立 runner 持有，manager 通过私有 runner socket 接管它。
- manager 异常重启后会扫描启用注册表和临时服务的 runtime 描述。manager 只接管仍在运行的
  runner，并保留服务 PID。
- runner 位于 `$HOME/.local/state/served/runtime/runners/<name>/`，持有服务进程、PTY、
  日志缓存、自动重启状态和 crash-loop 窗口。manager 异常退出不会停止它们。
- `served shutdown` 通过 graceful shutdown 停止所有 runner；`served disable` 和
  `served restart` 也会停止或替换对应 runner。升级使用 manager handoff 保留服务 PID；
  首次从旧 worker 架构升级时可能需要一次受控重启。
- system service 按安装用户的登录环境设置 `HOME`，并通过 login shell 启动 manager，
  因此 `/etc/profile` 等环境会在 manager 启动时被读取。manager 运行期间仍使用启动时的
  环境快照。
- system service 的工作目录使用安装用户的 home，不依赖 system manager 的 `%h` 展开。
- 每个 pipe 或 PTY 服务都有独立进程组。runner 先向整个进程组发送 `SIGTERM`，超时后发送
  `SIGKILL`，并确认服务 leader 已被回收。停止或重启失败会返回错误。manager 异常退出不会
  执行这条清理路径。
- `nohup`、后台化、daemonize 等脱离受管 shell 的子进程不在清理保证内。
- V1 不提供 root 模式、容器隔离、namespace、资源限制、依赖图或健康检查。

## 维护者构建

以下命令用于构建和检查 served 本身。部署个人项目时，不需要先安装 Rust 工具链。个人部署
应优先使用 Release 完整安装包。

```bash
make bootstrap       # 安装当前系统的 amd64 和 arm64 Rust target
make check           # 格式、Clippy 和本机测试
make msrv-check      # 使用 Rust 1.85 编译全部 target
make build-cross     # 构建当前系统的另一种架构
make build-all       # 构建当前系统的两种架构
make dist            # 打包当前系统的两种架构
make source-dist     # 生成确定性的 source 压缩包
make shellcheck      # 检查仓库中的 shell 脚本
make installer-check # 用 mock 测试在线安装器
make systemd-check   # 验证 systemd 模板
make launchd-check   # 验证 launchd 模板
make linux-check     # 在 Docker 中运行完整 Linux 检查
```

`make run` 使用 `.dev/` 下的隔离 `HOME` 启动 manager。另开终端后，可以运行
`make cli ARGS="list"` 或其他 served 命令。Linux 交叉发行固定使用 Zig 0.14.1 和
cargo-zigbuild 0.21.8。构建流程不跨操作系统：macOS 构建两种 macOS 架构，Linux 构建两种
Linux 架构。Docker 检查固定使用 Rust 1.85；本机构建和 CI 默认使用 stable，也可以通过
`RUST_TOOLCHAIN` 选择已经安装的 rustup 工具链。

核心需求记录在 [REQUIREMENTS.md](REQUIREMENTS.md)，技术决策记录在
[TECH-STACK.md](TECH-STACK.md)。

## 许可证

served 使用 [Unlicense](LICENSE) 发布。你可以自由使用、复制、修改、发布和分发本项目。
本软件不提供任何形式的保证。
