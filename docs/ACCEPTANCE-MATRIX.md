# served 验收矩阵

状态：当前开发版验收基线。

本文把 `REQUIREMENTS.md` 的 54 个验收场景映射到自动化 gate。`cargo test` 表示 Rust 单元
测试或集成测试。`release CI` 必须在目标操作系统或打包环境中执行。单台开发机的结果不能
替代该 gate。

| 场景 | Gate | 主要证据 |
| --- | --- | --- |
| 1 | `cargo test` | 配置文件优先级单元测试、`cli::edit_path_creates_template_without_editor`、`config_filename_cli` warning 测试 |
| 2 | `cargo test` | `manager_smoke::enable_restart_and_disable_a_pipe_service` |
| 3 | `cargo test` | `manager_smoke::enable_restart_and_disable_a_pipe_service` 验证重复名称失败且原链接不变 |
| 4 | `cargo test` | `manager_smoke::enable_restart_and_disable_a_pipe_service` |
| 5 | `cargo test` | manager smoke 的 restart 校验和 `config::json5_environment_overrides_legacy_dotenv_without_expansion` |
| 6 | `cargo test` | manager smoke 在无效 JSON5 后验证原进程仍运行 |
| 7 | `cargo test` | `config::restart_policies_are_distinct`、worker backoff 和 manager smoke |
| 8 | `cargo test` | `manager_smoke::pty_service_accepts_one_attach_session` |
| 9 | `cargo test` | `manager_smoke::pty_service_accepts_one_attach_session` |
| 10 | `cargo test` | `manager_smoke::manager_crash_keeps_runner_and_service_alive_for_adoption` |
| 11 | `cargo test` | `tui::tests::borderless_layout_adapts_and_preserves_service_states` |
| 12 | `cargo test` | `client::rejects_directory_without_managed_service` |
| 13 | `cargo test` | `tui::tests::actions_help_confirmation_and_errors_preserve_context` |
| 14 | `cargo test` | editor 优先级、`PATH` 候选顺序、CLI parser 和 `config_filename_cli` 路径测试 |
| 15 | `cargo test` | `cli::edit_path_creates_template_without_editor`、Clap 冲突定义 |
| 16 | `cargo test` | `config::template_does_not_rewrite_existing_source`、`config::template_keeps_deprecated_config_without_creating_current_file` |
| 17 | `cargo test` | `manager_smoke::direct_attach_supports_name_and_current_directory`、`tui::ctrl_c_is_the_attach_detach_byte` |
| 18 | `cargo test` | direct attach、client directory resolution 和 pipe attach 集成测试 |
| 19 | `cargo test` | direct attach PTY 集成测试和 TUI render tests |
| 20 | `cargo test` | `manager_smoke::pipe_service_supports_multiple_readonly_attach_sessions` |
| 21 | `cargo test` | persistent/memory history 集成测试和日志轮换单元测试 |
| 22 | `cargo test` | `manager_smoke::persistent_and_memory_history_survive_service_restarts` |
| 23 | `cargo test` | history chunk、logical line、sanitizer 和 raw persistence tests |
| 24 | `cargo test` | CLI parser、分页输出、JSON schema 和内存 history 集成路径 |
| 25 | `make systemd-check` | `tests/system_service_template.sh` 和 unit 静态断言 |
| 26 | `cargo test` | manager crash adoption 集成测试 |
| 27 | `cargo test`、`make systemd-check` | manager handoff/shutdown 集成测试和 `ExecReload` 检查 |
| 28 | `cargo test` | manager relinquish 集成测试 |
| 29 | `cargo test` | crash-loop attach 集成测试和结构化协议 round-trip |
| 30 | `cargo test` | crash-loop attach 与 memory history `--stdout`/`--json` 集成路径 |
| 31 | `cargo test` | 非交互 direct attach 集成路径和 TUI prompt model tests |
| 32 | `release CI` | macOS/Linux 双架构原生 runner 和同系统另一架构构建矩阵 |
| 33 | `release CI` | glibc 上限、macOS deployment target 和 ad-hoc 签名检查 |
| 34 | `cargo test` | supervisor lifecycle CLI parser、handoff、shutdown 和 relinquish tests |
| 35 | Linux release smoke | 两个 `served@<user>` 实例的 socket 和生命周期隔离 |
| 36 | Linux release smoke | `scripts/install.sh` 的旧 fixed unit 迁移路径 |
| 37 | Linux release smoke | `scripts/uninstall.sh` 的共享文件保留路径 |
| 38 | `cargo test` | CLI run parser、argv quoting 和 `manager_smoke::run_creates_a_full_temporary_service_without_reading_config_files` |
| 39 | `cargo test` | run 服务的 list/attach/history/restart/disable 与冲突集成路径 |
| 40 | `cargo test` | `manager_smoke::manager_crash_preserves_a_temporary_service_for_adoption` |
| 41 | `make launchd-check`、macOS release smoke | `plutil`、身份、HOME 和生命周期字段检查 |
| 42 | macOS release smoke | 活动 LaunchDaemon handoff、PID 保留、未加载状态和失败回滚 |
| 43 | macOS release smoke | 当前实例卸载、用户数据与其他实例共享文件保留 |
| 44 | `make installer-check` | 平台资产选择、checksum 失败和 `install.sh --yes` mock 测试 |
| 45 | `cargo test` | `config_filename_cli::explicit_file_creates_parents_and_preserves_existing_source`、`explicit_legacy_file_bypasses_discovery_and_deprecation` |
| 46 | `cargo test` | `config::explicit_source_resolves_cwd_and_environment_independently`、`manager_smoke::custom_sources_and_shared_workdirs_survive_recovery` |
| 47 | `cargo test` | `manager_smoke::custom_sources_and_shared_workdirs_survive_recovery`、`workdir_discovery_and_legacy_sources_remain_distinct` |
| 48 | `cargo test` | `manager_smoke::custom_sources_and_shared_workdirs_survive_recovery` |
| 49 | `cargo test` | `manager_smoke::workdir_discovery_and_legacy_sources_remain_distinct`、runner v1 wire tests |
| 50 | `cargo test` | `manager_smoke::start_stop_preserve_registration_history_and_reload_only_when_stopped` |
| 51 | `cargo test` | `manager_smoke::stopped_services_survive_adoption_and_all_services_return_after_shutdown`、`stopped_runner_replacement_does_not_launch_a_process`、`handoff_reaps_stopped_runners_and_preserves_manual_stop` |
| 52 | `cargo test` | `manager_smoke::stop_cancels_backoff_and_start_stop_handle_quick_exits`、runner 停止失败与有界事件队列测试 |
| 53 | `cargo test` | `manager::tests::old_runner_rejects_start_stop_without_receiving_a_mutating_request`、runner v1 能力缺省测试 |
| 54 | `cargo test` | CLI start/stop parser、共享目录歧义测试、`tui::tests::pending_operations_allow_navigation_help_and_quit_but_no_new_action` |

## 重写兼容 gate

- `protocol::run_request_keeps_the_v7_wire_shape` 固定未改变的 Run JSON。
- `protocol::enable_v8_preserves_independent_file_and_workdir` 固定 manager v8 Enable 字段。
- `runner_protocol::runner_status_keeps_the_v1_wire_shape` 固定既有 runner v1 status JSON。
- `runner_protocol::watch_status_is_an_additive_v1_request` 固定新增订阅仍属于 additive v1。
- `manager::watcher::falls_back_to_status_polling_for_an_older_v1_runner` 验证旧 runner 回退。
- `runner::server::watch_status_streams_the_initial_value_and_changes` 验证新 runner 推送路径。
- `worker::runtime::output_hub_pairs_snapshot_with_the_following_live_output` 验证 attach 快照与实时
  输出之间没有丢失窗口。
- `manager_smoke::persistent_and_memory_history_survive_service_restarts` 验证不持久化记录仍可
  分页导出为清理后的 stdout 和结构化 JSON，且不创建日志文件。

## 手册与 AI skill

- `make docs-check` 校验 mdoc、生成参考同步、skill 的可移植引用及独立包校验和。
- `tests/docs_install.sh` 在临时目录验证权限、补齐、故障回滚和只删除所属文件。
- macOS release smoke 校验 man 查询、文档修复保留 PID、升级回滚及多用户卸载保留。

## handoff 后的 runner 回收

- `process::tests::zombie_is_not_alive_even_when_identity_is_unavailable` 验证僵尸不阻止重建，包括 macOS 无法再查询身份的情况。
- Linux stat 解析测试区分僵尸、死亡与暂停、不可中断睡眠；缺失 PID 和启动时间不匹配仍拒绝。
- `manager::reaper` 测试验证只回收启动时捕获的子进程，不抢走 Tokio 新子进程的退出状态，
  并排除非子进程及不匹配的身份。
- `manager_smoke::handoff_reaps_stopped_runners_and_preserves_manual_stop` 覆盖 enabled/run：
  stop → handoff → 杀死 runner → 重建且保持停止 → start，以及第二次 handoff 后的 disable。
  测试要求旧 PID 被回收，不仅是业务进程已停止。

## 无框 TUI

`docs/TUI-DESIGN.md` 是页面和交互规范。`tui::tests` 与 `tui::view::tests` 覆盖窄屏、
Unicode、选择身份、断连保护、菜单/帮助/确认/错误返回、成功提示过期和异步动作限制。

`manager_smoke::tui_menu_drives_lifecycle_and_returns_from_attach` 在真实 PTY 中验证菜单、帮助、
start/stop、默认取消、attach 返回、历史阅读、禁用确认与退出。

## 无头构建与封装

- 默认构建与 `--no-default-features` 都运行 CLI 和后台集成测试；仅 TUI 菜单测试依赖 `tui`。
- `tests/cli_output.rs` 覆盖版本查询、参数错误封套、无副作用拒绝、非交互编辑、`--` 边界和断管。
- `tests/manager_smoke.rs` 覆盖 JSON 管理、历史枚举及旧 JSON 格式兼容；管道 attach 覆盖 EOF 后输出、原始控制字节、阻塞取消、断管和输出到 `/dev/null`。
- `tests/install_online.sh` 覆盖四个平台、两类资产、升级类型保留、显式切换、校验失败及缺失资产不回退。
- `tests/linux_variant_install.py` 在临时目录运行真实安装／回滚函数，仅替换 systemd 调用，验证两种构建接管时服务 PID 不变及失败回滚。
- `tests/macos_install_smoke.sh` 在 macOS CI 中验证两种包及双向切换；本机 Linux 不代替 macOS 验收。

## run 默认持久托管

- `run_cold_start_preserves_definition_and_disable_prevents_recovery` 覆盖 PTY/pipe、manager 和
  runner 同时丢失、restart=never、参数与环境保留、持久日志及 disable 后不恢复。
- `stopped_services_survive_adoption_and_all_services_return_after_shutdown` 覆盖手动停止的
  handoff/崩溃接管，以及正常 shutdown 后 enabled/run 均恢复。
- `legacy_run_migration_preserves_pid_and_retries_failed_publication` 覆盖迁移失败保留进程、
  重试、发布后中断的重复迁移，以及失效旧记录不复活。
- `run_missing_directory_retains_registration_and_reserves_name` 覆盖目录缺失时保留定义、
  阻止同名 run/enable，以及目录恢复后的再次启动。
- manager 单元测试覆盖持久记录权限、幂等发布和冲突拒绝；JSON/CLI 测试使用 schema v2。
