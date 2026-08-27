# served 需求

状态：产品需求草案。

`served` 是轻量的宿主机服务管理工具。它帮助个人开发者把个人非关键服务部署为长期运行
的服务。前台 manager 由安装用户选择的进程守护程序托管；systemd system unit 和 macOS
LaunchDaemon 是可选集成，不是核心运行时依赖。

个人非关键服务停止后，不会影响主机的登录和基础维护能力。机器人、Webhook、个人 API
和 Worker 适合使用 served。`sshd`、登录和网络等基础服务不适合使用 served。

served 只管理已经存在的项目目录。代码上传、构建和依赖安装不属于 served 的职责。本地
测试可以直接运行项目命令。served 负责部署后的常驻运行、重启、attach 和日志查看。

served 不是容器运行时，也不提供任意 root 服务管理、容器隔离、namespace、资源限制或
健康检查。

## 部署边界

- Linux/glibc 和 macOS 都提供 amd64、arm64 Release 产物。
- 进程守护程序必须以安装用户身份、该用户正常的 `HOME` 和前台 `served daemon` 启动
  manager。
- 通用优雅停止接口是 `served shutdown`；保留 runner 的 manager 切换接口是
  `served daemon --handoff`；跨 supervisor 迁移使用 `served daemon --relinquish`。
- Linux systemd 完整安装包提供 `install.sh`。脚本由目标安装用户运行，在需要时调用
  `sudo`，并启用运行 manager 的 `served@<user>.service`。
- macOS 完整安装包提供 LaunchDaemon 安装器。它安装
  `io.github.tunafish2k.served.<uid>`，开机后以目标安装用户身份运行 manager。
- 统一在线脚本检测 Linux/macOS 与 amd64/arm64，下载最新稳定 full 包和 SHA-256 sidecar，
  校验后安装。重复运行同一命令执行升级。
- 持久启用的项目服务必须先有自己的目录和 `.served.json5`，或兼容的旧版
  `.served.json`，再运行 `served enable`。
- 用户可以用 `served run -- <program> [args...]` 创建临时服务。临时服务不要求项目配置文件。
- `served enable` 启用项目服务并立即启动它。它不上传代码，也不执行构建。
- 项目文件或配置更新后，使用 `served restart` 应用变化。
- 服务异常时，使用 `served attach`、`served history` 或持久化日志排查。

## 核心模型

- 一个受管服务对应一个目录。同一目录同时最多有一个受管服务。
- 受管服务分为 `enabled` 和 `temporary`。已启用服务由服务配置文件和启用链接定义。
  临时服务由 `served run` 的命令行参数定义。
- 已启用服务的目录必须包含 JSON5 服务定义文件 `.served.json5`，或兼容的旧版
  `.served.json`。临时服务忽略这两个文件。
- 服务定义可以用 `env` 对象设置服务专用的字面量环境变量。
- 同一目录中的 `.env.served` 只作为旧版 dotenv 回退输入；新模板不会创建它。
- 项目 `.env` 与 served 无关，served 永远不会读取它。
- 服务工作目录始终是创建服务时的当前目录。
- 管理器通过用户拥有的启用链接发现已启用服务。管理器通过私有 runtime 描述接管仍有
  活动 runner 的临时服务。
- 服务名在所有受管服务中必须全局唯一。已启用服务的名称来自 JSON。临时服务的名称来自
  `--name` 或清洗后的当前目录名。
- 已启用服务需要改名时，先 `disable`，再修改名称，最后重新 `enable`。

启用注册表为：

```text
~/.config/served/enabled/<name> -> /path/to/service-directory
```

链接指向服务目录，而不是直接指向 JSON5 文件。这样管理器可以用统一方式加载服务配置、
旧版 `.env.served` 回退文件和工作目录。

## 配置

`.served.json5` 是一个服务对应的 JSON5 对象。它支持注释、不加引号的字段名、单引号、
双引号和尾逗号。最小结构如下：

旧文件名 `.served.json` 是弃用的兼容输入，内容仍按 JSON5 解析。配置文件选择规则固定为：

1. `.served.json5` 存在时使用它；`.served.json` 同时存在时忽略旧文件并输出 warning。
2. 只有 `.served.json` 时使用旧文件并输出弃用 warning，不自动复制、改名或删除。
3. `.served.json5` 无效时返回该文件的错误，不回退到 `.served.json`。
4. 两个文件都不存在时返回缺失配置错误。

manager 在恢复、enable 和 restart 时把 warning 写入 tracing 日志。`served edit` 把 warning
写入 stderr，`served edit --path` 的 stdout 仍只包含配置路径。旧文件名没有预定移除版本。

```json5
{
  name: "api",
  command: "python app.py",
  tty: true,
  syncRowsCols: true,
  restart: "never",
  persist_logs: false,
  env: {},
}
```

字段说明：

- `name`：必填。在所有启用服务中全局唯一。
- `command`：必填的 shell 命令字符串。可以包含多行，并作为多行 `/bin/sh -c` 脚本执行。
- `tty`：可选布尔值，默认 `true`。
- `syncRowsCols`：可选布尔值，默认 `true`。TTY attach 时，将服务 PTY 尺寸同步为
  attach 终端尺寸。`tty` 为 `false` 时仍保存该字段，但不生效。
- `restart`：可选重启策略，默认 `never`。
- 支持的重启策略为 `never`、`on-failure` 和 `always`。
- `persist_logs`：可选布尔值，默认 `false`。设置为 `true` 后，将完整 raw 输出保存到
  served 固定状态目录。
- `env`：可选的字面量字符串对象。值不会进行 shell 展开。JSON5 `env` 的同名值覆盖
  旧版 dotenv 值。

命令通过 `/bin/sh -c` 执行。`FOO=bar command` 这样的内联 shell 赋值不需要额外配置。
外部编辑器会直接打开源文件。JSON5 字符串中的 `\n` 会解码为实际换行；如果参数需要
字面量反斜杠-n，必须写成 `\\n`。

服务环境首先来自 system unit 的 login shell 环境。如果存在旧版 `.env.served`，它会
覆盖该环境；JSON5 的 `env` 对象再覆盖前两者。`.env.served` 使用标准 dotenv 规则解析，
不会作为 shell 脚本执行，也不支持任意文件路径或 `env_file` 配置。

管理器环境是启动时的快照。修改 `/etc/profile` 或其他 shell 启动文件后，运行中的服务
不会自动更新；需要重启管理器，或显式刷新其环境。JSON5 `env` 的值是字面量，旧版 dotenv
文件始终作为数据读取，不会被 shell source。

## 命令

### `served`

打开全局服务管理 TUI。它列出管理器已知的所有受管服务及其 `enabled` 或 `temporary` 类型。
与当前工作目录无关。

如果当前目录包含尚未启用的服务配置，TUI 可以显示如下提示：

```text
enable your service to manage it here!
```

未启用且未通过 `served run` 创建的目录不能由管理器控制。

### `served edit`

用外部编辑器打开当前目录中按上述规则选中的配置。两个文件都不存在时，served 会在
`.served.json5` 创建带注释的 JSON5 模板。已有文件会原样打开，served 不会重新格式化、
重写或迁移它。编辑只会修改文件，不会自动应用到运行中的服务。

编辑器按以下优先级解析：`-e/--editor COMMAND`、非空 `$EDITOR`，然后从 `PATH` 依次
查找 `editor`、`sensible-editor`、`nvim`、`vim`、`vi`、`nano`、`micro`、`hx`。命令可以
包含参数，配置路径会作为最后一个参数追加。`--path` 会创建缺失模板，并只打印配置的
绝对路径；它与 `--editor` 互斥。没有可用编辑器时，命令返回错误。

### `served enable`

只在服务目录中有效。

1. 按配置文件选择规则读取并校验 JSON5，包括可选的 `env` 对象和旧版 `.env.served` 回退。
2. 拒绝缺失或无效的配置。
3. 拒绝重复的全局服务名。
4. 创建用户级启用链接。
5. 在管理器中启动服务。

没有独立的 `start` 命令。

### `served run [options] -- <program> [args...]`

在当前目录创建临时服务。manager 必须已经运行。该命令不得读取或创建 `.served.json5`、
`.served.json` 和 `.env.served`，也不得创建启用链接。创建成功后，该命令必须只输出服务名
并返回。

- `--name` 可选，默认使用清洗后的当前目录名。
- 默认启用 TTY 和 PTY 尺寸同步。`--no-tty` 和 `--no-sync-rows-cols` 分别关闭这两个选项。
- `--restart` 接受 `never`、`on-failure`、`always`，默认 `never`。
- `--persist-logs` 默认关闭。`--log-max-bytes` 和 `--log-max-files` 的默认值与配置文件相同。
- `--env KEY=VALUE` 可以重复。字面量值覆盖 manager 环境快照中的同名键。一个键重复出现
  时，最后一个值生效。
- `--` 后必须至少有一个 UTF-8 参数。参数必须保持原始边界。served 不得解释 shell
  元字符。需要 shell 语法时，用户必须显式传入 `sh -c`。

临时服务必须支持 list、TUI、attach、history、restart 和 disable。进程退出后，服务必须
保持 `stopped` 状态。manager handoff、relinquish 或异常崩溃后，新 manager 必须接管仍在
运行的 runner。显式 shutdown、正常停止 manager 或主机重启后不得恢复临时服务。

### `served disable [name]`

停止并移除服务。不带名称时使用当前目录。提供名称后，可以从任意目录控制受管服务。
已启用服务同时删除启用链接。临时服务删除私有 runtime 描述。两种服务都保留持久化日志。

没有独立的 `stop` 命令。

### `served restart [name]`

不带名称时操作当前服务目录。提供名称后，可以从任意目录操作对应的受管服务。

重启已启用服务时，manager 必须按选择规则重新读取当前配置和旧版环境回退文件。manager
必须先完成校验，再停止并启动服务。没有独立的 `reload` 操作。

重启临时服务时，manager 必须使用创建时保存的命令、选项和环境。需要修改这些值时，用户
必须先 disable，再重新执行 `served run`。

校验必须在停止旧进程前完成。JSON5、`env` 或旧版 `.env.served` 无效时，保持当前运行的
服务不变，并报告错误。

### `served attach [name]`

直接连接运行中的 PTY 或管道服务，不打开服务管理 TUI。不带名称时，先规范化当前目录，
再匹配对应的受管服务。提供名称后，可以从任意目录连接受管服务。

目标服务必须正在运行。命令使用当前终端的 raw mode，并进入终端备用屏幕。进入后先显示
当前运行的 48 行清理快照，再流式显示实时输出。会话结束后恢复原来的 shell 屏幕和终端
模式。

对于 `tty: true`，会话向 PTY 转发输入，同时只允许一个客户端写入。对于 `tty: false`，
会话只读：在快照后转发 stdout 和 stderr raw 字节，忽略输入，并允许多个观察者。两种
会话中的 `Ctrl+C` 都只执行 detach，不会转发给服务。detach 不会停止或禁用服务。

对于 TTY 服务，客户端立即发送终端尺寸，并约每 250ms 轮询尺寸变化。`syncRowsCols` 为
`true` 时，运行器通过管理器控制 IPC 将尺寸应用到活动 PTY。尺寸控制连接失败可以恢复，
不会终止 raw attach；`syncRowsCols: false` 会让有效的尺寸请求不产生作用。

运行器在每个服务内维护最近 60 秒的失败窗口。至少 3 次非成功退出或 worker 启动、运行
错误会触发近期崩溃循环状态。手动 stop 和 restart 控制路径不计为失败。

如果 attach 发现服务未运行，且该状态达到阈值，运行器会通过管理器返回结构化的
attach-unavailable 响应。交互式 CLI attach 会警告，并在当前记录已持久化时询问
`Open latest.log? [y/N]`。TUI 会在提示区询问，`y` 或 `Enter` 打开，`n` 或 `Esc` 取消。
两者都使用与 `served edit` 相同的编辑器解析顺序，不会为内存历史创建临时文件，编辑器
退出后也不会重试 attach。非交互式 CLI 调用永远不会等待输入。

### `served list`

列出当前由管理器管理的服务。每个服务必须显示 `kind=enabled` 或 `kind=temporary`。

## 进程生命周期

- 服务由执行其命令的 shell 进程表示。
- `nohup`、`&`、daemonize 或类似方式创建的后代进程不在服务保证范围内，也不保证会被
  清理。
- 停止操作要求服务运行器向受管 shell 发送 `SIGTERM`。
- 如果进程在终止超时前没有退出，运行器发送 `SIGKILL`。
- `restart=never` 在服务退出后保持停止，直到显式 restart。
- `restart=on-failure` 在非成功退出后重启。
- `restart=always` 在每次退出后重启。
- 自动重启使用带最大延迟的指数退避，并持续重试。
- 手动 restart 会重置服务的重启尝试状态。
- 崩溃循环诊断独立于重启退避，保留 60 秒内的失败时间戳。该窗口由每个服务的运行器
  持有，因此普通管理器重启不会清除它。

## PTY 与 Attach

- 服务默认使用 PTY。
- 设置 `tty: false` 后可以关闭 PTY。
- TTY attach 默认同步 rows/cols。设置 `syncRowsCols: false` 后可以关闭。detach 后
  保留最后一次 attach 的尺寸；新建 PTY 从默认尺寸开始。
- TUI attach 接管 TUI 已持有的备用屏幕，清屏后显示服务；detach 后重绘服务管理器，
  不再嵌套另一个备用屏幕所有者。
- CLI 直接 attach 进入自己的备用屏幕，detach 后返回 shell。
- PTY attach 转发输入，并允许一个写入客户端。管道 attach 只转发 stdout/stderr raw
  字节，忽略输入，并允许多个观察者。
- Attach 先显示当前运行的清理后输出尾部。尾部可以跨多个输出事件聚合，然后接收实时
  字节。它不是终端状态回放，也不会发送给服务。
- Attach 中的 `Ctrl+C` 执行 detach，不会发送给服务。
- 服务管理 TUI 中、非 attach 会话里的 `Ctrl+C` 退出 TUI 客户端，不停止受管服务。
- 管理器 IPC 协议有版本号。Attach 尺寸控制使用当前协议，不与 raw PTY 流共享 frame。
- 协议版本 5 增加结构化 attach-unavailable 响应，包括服务名、近期失败次数、窗口长度
  和可选的持久化 `latest.log` 路径。

## 输出历史

- `.served.json5` 有可选的 `persist_logs` 布尔值，默认 `false`。
- 每次进程启动创建一条独立输出记录，包括自动重启和手动重启。
- TTY 和管道服务都会产生历史。TTY 输出是 raw PTY 字节；管道 stdout/stderr 按运行器
  收到的事件顺序合并。
- `persist_logs: true` 时，完整记录保存到 `$HOME/.local/state/served/logs/<name>/`。
- 当前记录为 `latest.log`；旧记录使用上一次运行的开始时间，格式为
  `YYYYMMDD-HHMMSS.log`，冲突时追加数字后缀。
- 持久化存储保留 100 个归档和一个 `latest.log`。目录权限为 `0700`，文件权限为 `0600`。
- `persist_logs: false` 时，当前记录和 100 个归档保存在运行器内存中，每条记录保留最后
  64 KiB。普通管理器重启和服务重启后仍可查看；终止运行器会清除它们。已有磁盘记录
  仍可查看。
- 持久化写入失败时记录 warning，并回退到内存，不停止服务。
- TUI 保留独立的历史列表和内容页。内容页按清理后的逻辑行显示 `current/total`；
  视觉换行不会改变总数。Attach 只增加当前运行的清理快照，不回放归档历史或终端状态。
- `served history [name]` 默认选择 `latest`，`--run <id>` 选择归档。没有输出模式时，
  命令按 `-e/--editor COMMAND`、`$EDITOR` 和系统编辑器的顺序打开持久化 raw 日志；
  `--path` 只打印持久化路径。`--stdout` 输出清理后的内容；`--json` 输出 `service`、
  `id`、`current`、`persisted`、`raw_bytes`、`total_lines` 和 `content`。这两种模式都支持
  持久化和内存记录，并通过分页 IPC 读取，不创建临时文件。`--path`、`--editor`、
  `--stdout` 和 `--json` 互斥。
- 历史内容通过分页的管理器 IPC 读取，显示前会清理 ANSI 和不安全控制序列。

## TUI

全局 TUI 提供：

- 受管服务列表、类型和当前状态；
- restart 操作；
- disable 操作；
- 面向 PTY 和管道服务的 attach 操作；
- 历史列表和可滚动的历史内容页，并显示逻辑行位置；
- 通过外部编辑器命令 `served edit` 编辑配置；
- 一行轮换显示的 tips：

```text
tips: <tip text>
```

tips 内置。每次启动 TUI 时随机选择一条；允许重复，不保存 tip 位置或其他管理器状态。

TUI 同时保留 `tips:` 和操作栏。没有选中服务时，操作栏显示导航和退出；选中服务后，
显示 restart、disable、attach 和 history。窄终端中操作栏换成两行，不会被截断。

`served edit` 是 CLI 编辑流程，不是 TUI 页面。生成的 JSON5 模板为每个字段写入行内说明，
因此外部编辑器是唯一的配置编辑入口。

## 管理器与安全边界

- manager 是前台进程，由外部进程守护程序负责启动和异常重启。manager 及其所有受管
  子进程都保持普通用户权限。
- 进程守护程序必须提供安装用户的规范 home；served 使用固定 HOME 路径，不读取 XDG 路径
  覆盖。
- 可选 systemd 模板使用 `User=%i`，不覆盖该用户主组，拒绝 root 实例，并使用用户 home
  作为工作目录。它不依赖 system manager 的 `%h` 展开，并在 `multi-user.target` 启用。
- 可选 LaunchDaemon 使用 `UserName` 和安装用户 home，不设置 `GroupName`。它由 system
  launchd 托管，不依赖用户登录会话，并设置 `AbandonProcessGroup` 保留独立 runner。
- TUI 和命令通过 `$HOME/.local/state/served/runtime/served.sock` 通信。
- socket 只允许该用户访问。
- 所有受管服务使用与管理器相同的用户身份。
- 不提供 root 模式、提权、容器隔离、namespace 策略、资源限制、依赖图或健康检查协议。
- 每个受管服务拥有独立运行器。manager 崩溃或被守护程序重启时，接管机制会保留 runner
  和服务进程。systemd 集成还使用 `KillMode=process`。明确的 shutdown、disable 或服务
  restart 会停止对应运行器。
- `served daemon` 与 system service 使用同一组固定的 HOME 路径。第二个 daemon 遇到已
  占用的 socket 时会拒绝接管。

## 可选 systemd 安装生命周期

- 安装脚本由目标普通用户运行，并在内部调用 `sudo`。它安装共享的
  `/usr/local/bin/served` 和 `/etc/systemd/system/served@.service`，不修改 shell 配置。
- 安装器从 passwd 解析目标用户 home，拒绝 root 和不能安全表示为实例名的用户。解析失败
  或 home 不存在时，必须在修改文件前退出。
- 一个实例对应一个普通用户。共享文件可以支持多个 enabled、active 或 stopped 实例；每个
  实例使用独立 HOME 状态和 socket。
- 主机首次建立 served systemd 集成时，安装器启用并启动调用用户的
  `served@<user>.service`。共享模板已经存在时，新增用户实例需要显式启用。覆盖共享文件前
  要求 `Y/n` 确认；非交互环境在需要确认时不修改文件或状态。
- 覆盖升级记录所有活动模板实例，并对每个实例执行 manager handoff。请求携带新客户端的
  绝对可执行文件路径。handoff 不停止 runner；单个实例失败时对它执行受控 restart。
- 覆盖升级保留每个已知实例的 active 和 enabled 状态。升级前停止的当前用户实例不被启动；
  安装器输出后续 start 或 enable 命令。
- 共享二进制、模板和用于迁移的固定 unit 先备份再替换。文件安装、handoff、迁移或目标
  状态应用失败时，脚本恢复旧文件并尝试恢复记录的服务状态。
- 安装器检测旧固定 `/etc/systemd/system/served.service`，读取并验证它的 `User=` 属于当前
  用户。活动 manager 优先通过旧客户端 handoff 到新二进制，再执行 relinquish，以状态 75
  退出且保留 runner；模板实例随后接管。失败时回退到受控 stop。
- 安装器同时检测旧 `~/.config/systemd/user/served.service` 和 `~/.local/bin/served`。
  只有模板实例达到目标状态后才删除旧 system/user 文件。自定义 XDG 目录只报告 warning，
  不自动复制或删除。
- 卸载使用 `y/N` 确认，只 disable 和停止当前用户的模板实例及属于该用户的旧 unit。停止
  失败时不删除相关文件；配置、状态和 shell 配置始终保留。
- 卸载后若还有其他 enabled 或 active 模板实例，必须保留共享二进制和模板。没有其他实例
  时，再用独立的 `y/N` 提示决定是否删除共享文件。

## 可选 LaunchDaemon 安装生命周期

- macOS 安装脚本由目标普通用户运行，并在内部调用 `sudo`。它安装共享的
  `/usr/local/bin/served` 和 `/Library/LaunchDaemons/io.github.tunafish2k.served.<uid>.plist`，
  不修改 shell 配置。
- 安装器从 Directory Service 解析 UID、规范 home 和登录 shell，并在修改文件前拒绝 root、
  无效账户、缺失 home 或不可执行 shell。
- plist 使用 `UserName`、`WorkingDirectory`、显式 HOME、登录 shell、`KeepAlive`、30 秒退出
  超时、私有 umask 和 `AbandonProcessGroup`。文件必须是 `root:wheel`、`0644`，并通过
  `plutil` 校验。
- 主机首次安装当前用户实例时，安装器清除 disabled override，bootstrap 并启动实例。已有但
  未加载的实例在升级后保持未加载。
- 共享二进制升级前记录所有活动 served LaunchDaemon。文件替换后，安装器以每个实例用户及
  HOME 执行 manager handoff，manager 和受管服务 PID 保持不变。
- 当前用户 plist 改变时，安装器先 disable 并让 manager relinquish，再 bootout、bootstrap、
  enable 和 kickstart。新 manager 接管保留的 runner。
- 二进制、plist、handoff 或 launchctl 操作失败时，安装器恢复旧文件，并把已升级的活动实例
  handoff 回稳定路径下的旧二进制。
- 卸载只 bootout、disable 和删除当前用户 plist，保留配置与状态。其他 LaunchDaemon 存在时
  保留共享二进制；没有其他实例时，再用独立 `y/N` 确认是否删除二进制。
- manager stdout/stderr 写入安装用户的 served state 目录。macOS 隐私保护可能要求用户为
  `/usr/local/bin/served` 授予 Full Disk Access，或把项目放在不受保护的目录。

## 统一在线安装生命周期

- 在线脚本只支持 Linux systemd 与 macOS launchd 的 amd64、arm64 主机。其他 Linux
  supervisor 继续使用 binary 资产手动安装。
- 脚本通过 GitHub 最新稳定 Release 选择 full 包，在临时目录下载 archive 和对应 SHA-256
  sidecar。checksum 失败时不得执行包内安装器或修改系统文件。
- 校验成功后，脚本以显式 `--yes` 调用包内安装器。相同文件重复安装是无操作；新版本沿用
  平台安装器的 handoff 和回滚语义。
- 不提供 `served update`、预发布选择、指定版本、后台检查或自动更新任务。

## 发行包边界

- GitHub Release 提供 Linux/macOS 平台二进制、systemd/LaunchDaemon 完整包和确定性的
  source archive；每个产物都有 SHA-256 sidecar。
- 仓库不维护发行版包管理器的元数据、模块或兼容性承诺。外部打包可以使用平台二进制或
  source archive，但不属于本项目的发行 gate。
- 当前不提供 runit、s6 或 supervisord 安装包；这些 init 可以直接托管前台
  `served daemon`，专用集成需要另行设计。

## V1 不做的事

- 代码上传、代码构建和依赖发布流程。
- `sshd`、登录、网络等主机基础服务的可用性保障。
- 兼容 Docker 的镜像或文件系统隔离。
- 除 served 自身非特权 unit 以外的任意 root/system 服务管理。
- 发行版包管理器元数据和模块。
- 一个目录或一个 JSON 文件中配置多个服务。
- 服务依赖或就绪检查。
- 公共 `served` CLI 中独立的 `start`、`stop` 或 `reload` 命令。
- 任意位置的 `.env.served` 文件。
- 自动发现无关进程或端口。
- runit、s6、supervisord 等其他守护程序的安装器或配置生成器。

## 验收场景

1. `served edit` 在空服务目录中创建带注释的 JSON5 `.served.json5`，且不创建 `.env.served`；
   只有 `.served.json` 时原地使用并输出弃用 warning，双文件时选择 `.served.json5` 并提示
   旧文件被忽略。
2. `served enable` 创建目录符号链接、启动服务，并使服务出现在全局 `served` 和
   `served list` 视图中。
3. 启用重复 `name` 时失败，且不替换已有链接。
4. `served disable` 删除链接并停止服务。
5. `served restart` 只有在完整校验后才应用当前 JSON5 和环境变化；JSON5 `env` 覆盖旧版
   `.env.served` 回退值。
6. 无效配置不会影响已经运行的服务。
7. `never`、`on-failure` 和 `always` 有可区分且可测试的行为。
8. PTY 服务可以 attach、detach 和 restart，且不会丢失管理器。
9. 第二个 attach 客户端不能向活动会话写入。
10. 替换管理器后，所有启用服务都能通过接管运行器恢复。
11. TUI 的 tips 行在每次启动时从内置 tips 中随机选择一条。
12. 未启用的服务目录不能由全局管理器控制。
13. 全局 TUI 操作栏根据是否选中服务显示不同内容，并为两种 `tty` 模式显示 attach 和
    history。
14. `served edit` 按 `-e/--editor COMMAND`、`$EDITOR` 和约定的 `PATH` 候选顺序打开选中的
    配置文件，并把配置路径作为最后一个参数追加。
15. `served edit --path` 创建缺失的带注释模板，并只打印绝对路径；`--path` 与 `--editor`
    互斥。
16. `served edit` 打开任一已有配置文件时不重写其中的源文本、注释或格式，也不自动迁移
    旧文件。
17. `served attach <name>` 进入运行中的 PTY 服务，不打开 TUI；`Ctrl-C` 退出会话但保持
    服务运行。
18. `served attach` 根据当前目录解析受管服务。未启用且未通过 `served run` 创建的目录会被
    拒绝。运行中的管道服务可以只读方式 attach。
19. 直接 attach 进入和退出备用屏幕并恢复 shell；TUI attach 返回完整重绘的管理器页面。
20. 多个管道观察者都能收到实时 raw 输出，管道输入不会到达受管服务。
21. 持久化历史写入 `latest.log`，按上一次运行的开始时间归档，并通过 TUI 和管理器读取
    同时提供 latest 和归档记录。
22. 非持久化历史不创建日志文件；运行器仍在时，服务重启和普通管理器重启都保留历史。
23. 历史内容按分页读取，报告准确的清理后逻辑行数；显示会移除 ANSI/控制序列，但不修改
    持久化 raw 文件。
24. `served history` 可以用编辑器或 `--path` 访问持久化 raw 日志；`--stdout` 和 `--json`
    通过分页 IPC 输出持久化或内存记录的清理后内容，且所有输出模式互斥。
25. 渲染后的 `served@.service` 通过 `systemd-analyze verify`，使用 `User=%i` 和用户 home，
    拒绝 root，不设置 `Group=`，并定义 `ExecStop`、`ExecReload`、`KillMode=process` 和状态
    75 的 relinquish 语义。
26. 终止管理器后，启用服务的运行器和服务 PID 仍存活；新管理器接管运行器，不重复启动
    服务。
27. `systemctl reload served@<user>` 从请求指定的新绝对路径替换管理器但不改变受管服务
    PID；`served shutdown` 在管理器退出前停止运行器。
28. `served daemon --relinquish` 让 manager 以状态 75 释放 socket，保留 runner；新
    supervisor 启动的 manager 接管相同服务 PID。
29. 60 秒内发生三次启动失败或非成功退出时，失败的 attach 返回结构化崩溃循环诊断；窗口
    外的失败不计入。
30. `persist_logs: true` 的崩溃循环服务会在 CLI 和 TUI attach 提示中提供当前 `latest.log`；
    `persist_logs: false` 不创建临时文件，并可通过 `served history --stdout` 或 `--json`
    读取运行器内存历史。
31. 关闭崩溃日志编辑器后，attach 仍然失败，且不会自动重试服务连接；非交互式 CLI attach
    永远不会等待输入。
32. macOS 和 Linux amd64、arm64 原生测试通过；每种宿主架构都能构建同一系统的另一架构。
33. Linux amd64、arm64 发行二进制最高依赖 GLIBC 2.17；macOS amd64、arm64 分别声明
    10.12 和 11.0 deployment target，并通过 ad-hoc 签名校验。
34. `served daemon` 可由非 systemd 守护程序前台托管；`served shutdown` 优雅停止所有
    runner，`served daemon --handoff` 替换 manager 并保留服务 PID。
35. 两个 systemd 用户实例同时运行时，各自 socket 归对应用户所有；停止一个实例不会影响
    另一个实例。
36. 安装器把当前用户的旧固定 system service 自动迁移到模板实例；可用时保留 runner，
    失败时明确报告受控停止。
37. 卸载一个用户实例时，如果其他实例仍 enabled 或 active，共享二进制和模板保持不变。
38. `served run -- <program> [args...]` 在没有有效配置文件时创建临时服务。该命令完整保留
    argv 边界，继承 manager 环境，并应用 CLI `--env` 覆盖。
39. 临时服务出现在 list 和 TUI 中。它支持 attach、history、restart 和按名称或目录
    disable。名称或目录冲突不得改变已有服务。
40. manager 异常退出后，新 manager 接管临时服务的 runner，且服务 PID 不变。正常
    shutdown 停止服务并删除私有 runtime 描述。主机重启后不得自动启动该服务。
41. macOS plist 通过 `plutil` 校验，以普通安装用户和规范 HOME 运行，并设置 KeepAlive、
    退出超时、私有 umask 与 `AbandonProcessGroup`。
42. macOS 覆盖升级 handoff 所有活动 LaunchDaemon；服务 PID 不变，未加载实例保持未加载，
    任一步失败时恢复旧二进制、plist 和活动 manager。
43. macOS 卸载当前用户实例时保留配置、状态和其他用户需要的共享二进制。
44. 在线安装器正确选择平台 full 包，验证 SHA-256 后才调用 `install.sh --yes`；checksum
    失败和不支持的平台或架构不会执行安装器。
