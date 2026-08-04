# served 需求

状态：产品需求草案。

`served` 是轻量的宿主机服务管理工具。它帮助个人开发者把个人非关键服务部署为长期运行
的服务。前台 manager 由安装用户选择的进程守护程序托管；systemd system unit 是可选的
Linux 集成，不是核心运行时依赖。

个人非关键服务停止后，不会影响主机的登录和基础维护能力。机器人、Webhook、个人 API
和 Worker 适合使用 served。`sshd`、登录和网络等基础服务不适合使用 served。

served 只管理已经存在的项目目录。代码上传、构建和依赖安装不属于 served 的职责。本地
测试可以直接运行项目命令。served 负责部署后的常驻运行、重启、attach 和日志查看。

served 不是容器运行时，也不提供任意 root 服务管理、容器隔离、namespace、资源限制或
健康检查。

## 部署边界

- macOS 和 Linux/glibc 分别提供 amd64、arm64 Release 产物。
- 进程守护程序必须以安装用户身份、该用户正常的 `HOME` 和前台 `served daemon` 启动
  manager。
- 通用优雅停止接口是 `served shutdown`；保留 runner 的 manager 切换接口是
  `served daemon --handoff`。
- Linux systemd 完整安装包提供 `install.sh`。脚本由目标安装用户运行，在需要时调用
  `sudo`，并启用运行 manager 的 `served.service`。
- 项目服务必须先有自己的目录和 `.served.json`，再运行 `served enable`。
- `served enable` 启用项目服务并立即启动它。它不上传代码，也不执行构建。
- 项目文件或配置更新后，使用 `served restart` 应用变化。
- 服务异常时，使用 `served attach`、`served history` 或持久化日志排查。

## 核心模型

- 一个服务对应一个目录。
- 目录包含 JSON5 服务定义文件 `.served.json`。
- 服务定义可以用 `env` 对象设置服务专用的字面量环境变量。
- 同一目录中的 `.env.served` 只作为旧版 dotenv 回退输入；新模板不会创建它。
- 项目 `.env` 与 served 无关，served 永远不会读取它。
- 服务工作目录始终是配置目录。
- 管理器通过用户拥有的启用链接发现服务。
- 服务名来自 JSON 的 `name` 字段，且在所有启用服务中必须全局唯一。
- 已启用服务需要改名时，先 `disable`，再修改名称，最后重新 `enable`。

启用注册表为：

```text
~/.config/served/enabled/<name> -> /path/to/service-directory
```

链接指向服务目录，而不是直接指向 JSON 文件。这样管理器可以用统一方式加载
`.served.json`、旧版 `.env.served` 回退文件和工作目录。

## 配置

`.served.json` 是一个服务对应的 JSON5 对象。它支持注释、不加引号的字段名、单引号、
双引号和尾逗号。最小结构如下：

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

打开全局服务管理 TUI。它列出管理器已知的所有启用服务，与当前工作目录无关。

如果当前目录包含尚未启用的服务配置，TUI 可以显示如下提示：

```text
enable your service to manage it here!
```

未启用的服务不能由管理器控制。

### `served edit`

用外部编辑器打开当前目录的 `.served.json`。如果文件不存在，served 会先创建带注释的
JSON5 模板。已有文件会原样打开，served 不会重新格式化或重写它。编辑只会修改文件，
不会自动应用到运行中的服务。

`-e/--editor COMMAND` 覆盖 `$EDITOR`。命令可以包含参数，配置路径会作为最后一个参数
追加。`--path` 会创建缺失模板，并只打印配置的绝对路径；它与 `--editor` 互斥。没有
可用编辑器时，命令返回错误。

### `served enable`

只在服务目录中有效。

1. 读取并校验 JSON5 `.served.json`，包括可选的 `env` 对象和旧版 `.env.served` 回退。
2. 拒绝缺失或无效的配置。
3. 拒绝重复的全局服务名。
4. 创建用户级启用链接。
5. 在管理器中启动服务。

没有独立的 `start` 命令。

### `served disable [name]`

删除启用链接并停止服务。不带名称时使用当前目录；提供名称后，可以从任意目录控制该
启用服务。

没有独立的 `stop` 命令。

### `served restart [name]`

不带名称时操作当前服务目录。提供名称后，可以从任意目录操作对应的启用服务。

重启总是重新读取当前 `.served.json` 和旧版环境回退文件，完成校验后再停止并启动服务。
没有独立的 `reload` 操作。

校验必须在停止旧进程前完成。JSON5、`env` 或旧版 `.env.served` 无效时，保持当前运行的
服务不变，并报告错误。

### `served attach [name]`

直接连接运行中的 PTY 或管道服务，不打开服务管理 TUI。不带名称时，先规范化当前目录，
再匹配对应的启用服务。提供名称后，可以从任意目录连接启用服务。

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
两者都使用 `$EDITOR`，不会为内存历史创建临时文件，编辑器退出后也不会重试 attach。
非交互式 CLI 调用永远不会等待输入。

### `served list`

列出当前由管理器运行的服务。

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

- `.served.json` 有可选的 `persist_logs` 布尔值，默认 `false`。
- 每次进程启动创建一条独立输出记录，包括自动重启和手动重启。
- TTY 和管道服务都会产生历史。TTY 输出是 raw PTY 字节；管道 stdout/stderr 按运行器
  收到的事件顺序合并。
- `persist_logs: true` 时，完整记录保存到 `$HOME/.local/state/served/logs/<name>/`。
- 当前记录为 `latest.log`；旧记录使用上一次运行的开始时间，格式为
  `YYYYMMDD-HHMMSS.log`，冲突时追加数字后缀。
- 持久化存储保留 100 个归档和一个 `latest.log`。目录权限为 `0700`，文件权限为 `0600`。
- `persist_logs: false` 时，当前记录和 100 个归档保存在运行器内存中。普通管理器重启
  后仍可查看；显式停止服务或终止运行器会清除它们。已有磁盘记录仍可查看。
- 持久化写入失败时记录 warning，并回退到内存，不停止服务。
- TUI 保留独立的历史列表和内容页。内容页按清理后的逻辑行显示 `current/total`；
  视觉换行不会改变总数。Attach 只增加当前运行的清理快照，不回放归档历史或终端状态。
- `served history [name]` 使用 `$EDITOR` 打开持久化 `latest.log`；`--run <id>` 选择
  归档，`-e/--editor COMMAND` 优先于 `$EDITOR`，`--path` 只打印选中日志的路径。
  `--path` 与 `--editor` 互斥。内存记录没有 CLI 路径，会提示使用 TUI 浏览器或开启
  持久化。
- 历史内容通过分页的管理器 IPC 读取，显示前会清理 ANSI 和不安全控制序列。

## TUI

全局 TUI 提供：

- 启用服务列表和当前状态；
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
- 可选 systemd unit 使用 `User=<installation user>`、该用户主组和 home 工作目录，不依赖
  system manager 的 `%h` 展开，并在 `multi-user.target` 启用。
- TUI 和命令通过 `$HOME/.local/state/served/runtime/served.sock` 通信。
- socket 只允许该用户访问。
- 所有受管服务使用与管理器相同的用户身份。
- 不提供 root 模式、提权、容器隔离、namespace 策略、资源限制、依赖图或健康检查协议。
- 每个启用服务拥有独立运行器。管理器接管机制允许 manager 崩溃或被守护程序重启时保留
  runner 和服务进程；systemd 集成额外使用 `KillMode=process`。明确的 shutdown、
  disable 或服务 restart 会停止对应运行器。
- `served daemon` 与 system service 使用同一组固定的 HOME 路径。第二个 daemon 遇到已
  占用的 socket 时会拒绝接管。

## 可选 systemd 安装生命周期

- 安装脚本由目标用户运行，并在内部调用 `sudo`。它安装
  `/usr/local/bin/served` 和 `/etc/systemd/system/served.service`，不修改用户 shell 配置。
- 安装器从 passwd 解析目标用户的 home。如果无法解析或目录不存在，会在修改文件前失败。
- 全新安装不询问升级确认。
- 只要目标二进制或 system unit 任一已存在，安装就按覆盖升级处理，并在修改文件前询问
  确认。这也覆盖修复不完整安装的情况。
- 活动管理器进行覆盖升级时，安装器会询问是否通过 `systemctl reload` 应用新管理器。
  默认 `Y`，执行 manager handoff，不停止运行器或受管服务。
- handoff 失败时，安装器执行受控的 `systemctl restart`，并报告受管服务已重启。拒绝
  handoff 时，新文件仍会安装，但旧管理器继续运行，直到稍后 reload。
- 安装器不会悄悄留下使用旧可执行文件的活动管理器而不报告状态。
- 卸载会询问确认。确认后先 disable system service；如果服务仍在运行，再停止它。只有
  两个操作都成功后，才删除已安装的 unit 和二进制文件。
- 已经 disable 的服务或不存在的 unit 可以满足卸载的 disable 步骤；只有真正的 systemd
  失败才会中止清理。
- 卸载不修改用户 shell 配置，包括旧版本可能写入的 PATH 片段。
- 确认提示要求交互式终端。非交互执行会中止，且不改变服务状态或文件。
- 如果 disable 成功但停止活动服务失败，卸载会在删除任何文件前中止。
- 卸载只有一个确认提示。确认后，disable 和 stop 不再重复询问。
- 全新安装失败时，脚本会删除本次创建的文件和服务启用状态，然后退出。
- 升级 handoff 使用 `Y/n`，默认同意。卸载使用 `y/N`，必须明确输入 `y`。
- 全新安装直接启用并启动 system service，不询问 handoff。安装后的 handoff 提示只适用于
  正在运行的活动服务覆盖升级。
- 覆盖升级会保留 inactive 服务的停止状态，不会启动升级前已经停止的服务。
- 如果升级后服务保持停止，安装器会按 enabled 状态输出对应的恢复命令。
- 覆盖升级保留服务原有的 enabled 或 disabled 状态，不调用 `enable` 或 `disable`；只有
  全新安装会启用 system service。
- 安装或升级成功后，脚本会输出全局二进制路径，不需要 export 命令。
- 文件替换具有事务性：保存旧文件后，如果安装新二进制或 unit 失败，脚本会恢复两者。
- 如果升级后的受控重启失败，脚本会恢复旧二进制和 unit，并尝试启动旧管理器。回滚也失败
  时，会同时报告两个错误。
- 安装时会检测旧的 `~/.config/systemd/user/served.service` 或 `~/.local/bin/served`。
  确认迁移后，先 disable 并停止旧 user service，再安装新的 system service；只有新服务
  active 后才删除旧文件。
- 如果无法联系旧 user manager，迁移会中止，不删除旧文件。自定义 XDG 目录只报告 warning，
  不会自动复制或删除。

## V1 不做的事

- 代码上传、代码构建和依赖发布流程。
- `sshd`、登录、网络等主机基础服务的可用性保障。
- 兼容 Docker 的镜像或文件系统隔离。
- 除 served 自身非特权 unit 以外的任意 root/system 服务管理。
- 一个目录或一个 JSON 文件中配置多个服务。
- 服务依赖或就绪检查。
- 公共 `served` CLI 中独立的 `start`、`stop` 或 `reload` 命令。
- 任意位置的 `.env.served` 文件。
- 自动发现无关进程或端口。
- launchd、runit、s6、supervisord 等守护程序的安装器或配置生成器。

## 验收场景

1. `served edit` 在空服务目录中创建带注释的 JSON5 `.served.json`，且不创建 `.env.served`。
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
14. `served edit` 使用 `-e/--editor COMMAND` 优先于 `$EDITOR` 打开 `.served.json`，并把
    配置路径作为最后一个参数追加。
15. `served edit --path` 创建缺失的带注释模板，并只打印绝对路径；`--path` 与 `--editor`
    互斥。
16. `served edit` 打开已有 `.served.json` 时不重写其中的源文本、注释或格式。
17. `served attach <name>` 进入运行中的 PTY 服务，不打开 TUI；`Ctrl-C` 退出会话但保持
    服务运行。
18. `served attach` 根据当前目录解析启用服务；未启用目录会被拒绝，运行中的管道服务
    可以只读方式 attach。
19. 直接 attach 进入和退出备用屏幕并恢复 shell；TUI attach 返回完整重绘的管理器页面。
20. 多个管道观察者都能收到实时 raw 输出，管道输入不会到达受管服务。
21. 持久化历史写入 `latest.log`，按上一次运行的开始时间归档，并通过 TUI 和管理器读取
    同时提供 latest 和归档记录。
22. 非持久化历史不创建日志文件；运行器仍在时，服务重启和普通管理器重启都保留历史。
23. 历史内容按分页读取，报告准确的清理后逻辑行数；显示会移除 ANSI/控制序列，但不修改
    持久化 raw 文件。
24. `served history` 使用 `-e/--editor` 优先于 `$EDITOR` 打开选中的持久化 raw 日志；
    `--path` 只打印路径，内存记录清晰报告失败原因。
25. 渲染后的 system unit 通过 `systemd-analyze verify`，使用安装用户 home 作为
    `WorkingDirectory`，不依赖 system manager 的 `%h` home，并定义 `ExecStop`、
    `ExecReload` 和 `KillMode=process`。
26. 终止管理器后，启用服务的运行器和服务 PID 仍存活；新管理器接管运行器，不重复启动
    服务。
27. `systemctl reload served` 替换管理器但不改变受管服务 PID；`served shutdown` 在管理器
    退出前停止运行器。
28. 活动安装覆盖升级时提供 manager handoff；拒绝时报告状态，handoff 失败时回退到受控
    重启。
29. 60 秒内发生三次启动失败或非成功退出时，失败的 attach 返回结构化崩溃循环诊断；窗口
    外的失败不计入。
30. `persist_logs: true` 的崩溃循环服务会在 CLI 和 TUI attach 提示中提供当前 `latest.log`；
    `persist_logs: false` 不创建临时文件，只报告内存历史选项。
31. 关闭崩溃日志编辑器后，attach 仍然失败，且不会自动重试服务连接；非交互式 CLI attach
    永远不会等待输入。
32. macOS 和 Linux 的 amd64、arm64 原生测试通过；每种宿主架构都能构建同操作系统的另一
    架构。
33. Linux amd64、arm64 发行二进制最高依赖 GLIBC 2.17；macOS amd64、arm64 分别声明
    10.12 和 11.0 deployment target，并通过 ad-hoc 签名校验。
34. `served daemon` 可由非 systemd 守护程序前台托管；`served shutdown` 优雅停止所有
    runner，`served daemon --handoff` 替换 manager 并保留服务 PID。
