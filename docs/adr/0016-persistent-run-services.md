# ADR 0016：run 默认持久托管

- 状态：已接受
- 日期：2026-09-25
- 替代：ADR 0011 的临时生命周期与存储边界；调整 ADR 0015 的 run 冷启动恢复行为。

## 背景

用户将 `served run` 理解为无需编写项目配置的长期托管入口。旧实现只接管存活 runner，
主机重启或正常 shutdown 后丢失服务，与这一预期不符。

## 决策

- run 和 enable 的区别是定义来源，两者都在 manager 冷启动时恢复。run 不读取项目配置。
- run 定义位于 `$HOME/.config/served/run/<name>.json`。版本 1 保存启动定义及完整环境快照，
  使用私有目录 `0700`、文件 `0600`，写入并同步临时文件后原子发布，不能覆盖冲突记录。
- shutdown 停止进程并保留定义；stop 停止本次运行；disable 删除定义并取消后续恢复。
- manager handoff、relinquish 和崩溃接管保留进程及存活 runner 的手动停止状态。
  主机重启或 shutdown 后，手动停止状态不保留。
- `--restart` 只控制业务进程退出后的行为，仍默认 never；冷启动恢复与此策略无关。
  日志、TTY 等默认值不变。本次不增加临时运行参数。
- 缺失目录或恢复失败时保留记录并报告错误，修复后重新启动 manager 可重试。
- 升级仅迁移存活且定义匹配的旧临时服务。先发布持久记录，再移除旧记录，保留 PID。
  已发布的相同记录允许重复迁移；写入失败保留旧记录和原进程，后续 manager 启动重试。
  没有存活 runner 的旧记录不会转为持久托管。
- 服务类型由 temporary 改为 run。manager 协议为 v10，JSON 封套 schema 为 2；
  runner 协议保持 v1。二次封装调用方需适配新 schema 和 kind。

## 结果

用户不必为跨主机重启恢复另写配置或执行保存命令。一次性命令也会在下次 manager 冷启动时
再次运行；不希望恢复时必须 disable。回退到旧 manager 不会读取新 run 注册目录，回退期间
不承诺这些服务的恢复；记录保留供新 manager 使用。

仅 handoff/relinquish 在握手阶段兼容旧 manager v9，确保升级时保留 runner；普通服务操作不降级。
