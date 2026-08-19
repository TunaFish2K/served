# ADR 0009：使用每用户 systemd 实例和共享安装

- 状态：已接受
- 日期：2026-08-04

## 背景

固定的 `/etc/systemd/system/served.service` 把一台主机限制为一个安装用户，并让共享
二进制的所有权和可选 systemd 集成难以独立管理。

共享二进制升级还有额外约束：多个用户的 manager 可能同时活动。升级必须把每个活动
manager 切换到新路径，同时保留各自的 runner；从旧固定 unit 迁移时，新旧 supervisor
名称不同，不能依赖普通 reload 完成所有权转移。

## 决策

- systemd 集成使用 `served@.service`。实例名是普通非 root 用户名，unit 使用 `User=%i`，
  不设置 `Group=`，由 systemd 使用账户主组。`ExecCondition` 拒绝 `root` 实例。
- `/usr/local/bin/served` 和模板是主机共享文件；每个 `served@<user>.service` 拥有独立的
  HOME、公共 socket、启用 registry、runner 和受管服务。
- reload 客户端把自身可执行文件的绝对路径放入协议版本 6 的 handoff 请求。manager 只
  接受可执行普通文件，再 `exec` 指定路径；因此二进制被替换或迁移到新路径时都可用。
- `served daemon --relinquish` 只释放 manager socket，不停止 runner，并让前台 manager 以
  状态 75 退出。模板把 75 配置为成功状态和 `RestartPreventExitStatus`，用于跨 unit 迁移。
- 完整包安装器由目标普通用户运行。它记录所有现有实例状态，原子替换共享文件，reload
  每个活动实例，并保留停止或 disabled 状态。失败时恢复文件和已记录状态。
- 安装器自动迁移属于当前用户的旧固定 system unit 和旧 user unit。活动固定 manager
  优先通过旧协议 handoff 到新二进制，再 relinquish，由模板实例接管 runner；无法转移时
  才执行受控停止。新实例达到目标状态前不删除旧文件。
- 卸载器只停止并 disable 当前用户的实例。存在其他 enabled 或 active 实例时，必须保留
  共享二进制和模板；没有其他实例时，再单独确认是否删除共享文件。用户配置和状态始终保留。
- 当前只维护仓库内的可选 systemd 集成。未来的 runit、s6 或其他 init 集成必须按对应
  生命周期语义单独设计，不建立一个行为含糊的通用 init 包。
- 发行版包管理器元数据和模块不属于仓库维护范围。外部打包者可以消费平台二进制或 source
  archive，但不会形成额外的兼容性和 CI 承诺。

## 结果

一台主机可以用一个共享二进制服务多个普通用户，同时保持状态和控制 socket 完全按 HOME
隔离。Linux 用户可以只使用二进制，也可以选择完整包中的 systemd 集成。

共享升级需要遍历所有活动实例，安装和回滚逻辑比固定 unit 更复杂。显式 stop 或 restart
仍然会停止该用户的 runner；只有 handoff、异常 manager 退出和专用 relinquish 保留它们。
其他 init 系统仍可直接托管 `served daemon`，但仓库不承诺未实现的安装器。
