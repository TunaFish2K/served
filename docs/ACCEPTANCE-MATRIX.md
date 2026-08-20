# served 验收矩阵

状态：v0.6.0 验收基线。

本文把 `REQUIREMENTS.md` 的 40 个验收场景映射到自动化 gate。`cargo test` 表示 Rust 单元
测试或集成测试。`release CI` 必须在目标操作系统或打包环境中执行。单台开发机的结果不能
替代该 gate。

| 场景 | Gate | 主要证据 |
| --- | --- | --- |
| 1 | `cargo test` | `config::template_creates_annotated_json5_without_legacy_env_file`、`cli::edit_path_creates_template_without_editor` |
| 2 | `cargo test` | `manager_smoke::enable_restart_and_disable_a_pipe_service` |
| 3 | `cargo test` | `manager_smoke::enable_restart_and_disable_a_pipe_service` 验证重复名称失败且原链接不变 |
| 4 | `cargo test` | `manager_smoke::enable_restart_and_disable_a_pipe_service` |
| 5 | `cargo test` | manager smoke 的 restart 校验和 `config::json5_environment_overrides_legacy_dotenv_without_expansion` |
| 6 | `cargo test` | manager smoke 在无效 JSON5 后验证原进程仍运行 |
| 7 | `cargo test` | `config::restart_policies_are_distinct`、worker backoff 和 manager smoke |
| 8 | `cargo test` | `manager_smoke::pty_service_accepts_one_attach_session` |
| 9 | `cargo test` | `manager_smoke::pty_service_accepts_one_attach_session` |
| 10 | `cargo test` | `manager_smoke::manager_crash_keeps_runner_and_service_alive_for_adoption` |
| 11 | `cargo test` | TUI model/render tests；随机选择由 `rand` 启动路径执行 |
| 12 | `cargo test` | `client::rejects_directory_without_managed_service` |
| 13 | `cargo test` | `tui::main_footer_describes_available_actions`、`tui::main_render_keeps_tip_and_contextual_footer` |
| 14 | `cargo test` | editor 优先级、`PATH` 候选顺序和 CLI parser tests |
| 15 | `cargo test` | `cli::edit_path_creates_template_without_editor`、Clap 冲突定义 |
| 16 | `cargo test` | `config::template_does_not_rewrite_existing_source` |
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
| 32 | `release CI` | 两个 Linux 原生 runner 和另一架构构建矩阵 |
| 33 | `release CI` | `scripts/verify-release-binary.sh` 的 glibc 检查 |
| 34 | `cargo test` | supervisor lifecycle CLI parser、handoff、shutdown 和 relinquish tests |
| 35 | Linux release smoke | 两个 `served@<user>` 实例的 socket 和生命周期隔离 |
| 36 | Linux release smoke | `scripts/install.sh` 的旧 fixed unit 迁移路径 |
| 37 | Linux release smoke | `scripts/uninstall.sh` 的共享文件保留路径 |
| 38 | `cargo test` | CLI run parser、argv quoting 和 `manager_smoke::run_creates_a_full_temporary_service_without_reading_config_files` |
| 39 | `cargo test` | 临时服务的 list/attach/history/restart/disable 与冲突集成路径 |
| 40 | `cargo test` | `manager_smoke::manager_crash_preserves_a_temporary_service_for_adoption` |

## 重写兼容 gate

- `protocol::manager_request_keeps_the_v7_wire_shape` 固定公共 manager v7 JSON。
- `runner_protocol::runner_status_keeps_the_v1_wire_shape` 固定既有 runner v1 status JSON。
- `runner_protocol::watch_status_is_an_additive_v1_request` 固定新增订阅仍属于 additive v1。
- `manager::watcher::falls_back_to_status_polling_for_an_older_v1_runner` 验证旧 runner 回退。
- `runner::server::watch_status_streams_the_initial_value_and_changes` 验证新 runner 推送路径。
- `worker::runtime::output_hub_pairs_snapshot_with_the_following_live_output` 验证 attach 快照与实时
  输出之间没有丢失窗口。
- `manager_smoke::persistent_and_memory_history_survive_service_restarts` 验证不持久化记录仍可
  分页导出为清理后的 stdout 和结构化 JSON，且不创建日志文件。
