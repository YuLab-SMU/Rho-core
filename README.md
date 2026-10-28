# Rho Core

Rho Core 为 Agent 的工作环境补足需要管理的本地执行：让一次运行脱离等待者存活，以
调用方持有的身份保护重复送达，找回原请求、执行事实与有界输出，并在 Core 重启后接续
而不重跑。它也保留选定的外部 Agent 活动，供重新读取和后处理。Agent 在自己的环境中
阅读文档、编写与组合代码、直接使用已有的 CLI、SDK 和 MCP；只有存在明确执行缺口的
动作才进入 Core。模型循环、协议兼容与科研方法由各自的 Owner 承担。

`rho-core` 是支持 Unix 与 Windows 的 Rust 库与二进制。受理与执行事实保存在 SQLite 事务中，
脚本与输出是内容寻址文件；`rho-core serve` 常驻宿主让 Agent 多次独立的 shell 调用使用
同一组受管运行。尚无公共 SDK、网络或 MCP 接入。各部分的现状见 [进度](docs/PROGRESS.md)。

| 文档 | 内容 |
| --- | --- |
| [受管运行契约](docs/MANAGED-RUN.md) | 调用、身份与去重、持有与停止、限额、存储与重启、不提供的保证 |
| [信息模型](docs/INFORMATION-MODEL.md) | 请求键、独立事实、事务边界、单调更新与删除保护 |
| [宿主与 CLI](docs/HOST-CLI.md) | Agent 跨调用提交、等待、读取与取消；退出码、宿主限额与故障语义 |
| [活动观测契约](docs/ACTIVITY.md) | 外部来源身份、事务追加、去重、保留缺口与游标续读 |
| [架构](docs/ARCHITECTURE.md) | Core 为什么存在、职责分工、何时使用、不随实验改变的边界 |
| [工程方法](docs/ENGINEERING.md) | 怎样从真实结果推进工作：阶段、迭代、分析、检查与记录 |
| [进度](docs/PROGRESS.md) | 各部分所处阶段、已验证范围、开放问题、决定与任务记录 |
| [SECURITY](SECURITY.md) / [PRIVACY](PRIVACY.md) | 执行边界与数据处理 |
| [工作约束](AGENTS.md) | 在本仓库工作时遵循的规则 |

可运行调用见 [`examples/managed_run.rs`](examples/managed_run.rs)；行为回归在
[`tests/`](tests/)：[`managed_run.rs`](tests/managed_run.rs)、[`restart.rs`](tests/restart.rs)、
[`storage.rs`](tests/storage.rs)、[`host.rs`](tests/host.rs) 与 [`activity.rs`](tests/activity.rs)。
[可独立核验的分析试点](examples/scientific-pilot/README.md)使用真实 Iris 数据与独立数学
核验。Agent 评估在 Rho-plugins 的
[评估说明](../Rho-plugins/plugins/agent-eval/core-agent/README.md)，Claude Code 与 dsh 的活动
适配器在 [Agent 观测示例](../Rho-plugins/examples/agent-observation/README.md)。

文档中的“首个受管流程”指 Core 自身的首个程序调用闭环，与
[Rho](https://github.com/YuLab-SMU/Rho) 文档中的 first integrated flow（应用层首个整体
用户流程）不是同一件事，两者各自验收。

许可证见 [LICENSE](LICENSE)。

## 构建与检查

工具链由 [`rust-toolchain.toml`](rust-toolchain.toml) 固定。回归在 Unix 上使用 `/bin/sh`、
`mkfifo` 与 `perl`，在 Windows 上使用 Git Bash 的 MSYS `sh.exe`（含其 `awk`/`perl`），
并在临时目录中创建和清理夹具进程。Cargo 命令依次运行，不并行：

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo run --locked --example managed_run
```

`cargo test` 同时构建并检查 `rho-core` 二进制；`cargo run --locked --bin rho-core -- help` 打印其用法。

Linux 的真实 UID、proc/PID 视图与内核重启验证另用 [`tests/linux_platform.py`](tests/linux_platform.py)。
它以 root 监督普通 UID 的独立宿主，明确接收已构建二进制和 `/var/tmp` 下的持久夹具目录；
使用 Python 3、`setpriv`、`unshare`、`nsenter` 与 `mount`：

```sh
sudo python3 tests/linux_platform.py uid --binary target/debug/rho-core --root /var/tmp/rho-core-platform-check
sudo python3 tests/linux_platform.py namespace --binary target/debug/rho-core --root /var/tmp/rho-core-platform-check
sudo python3 tests/linux_platform.py prepare-reboot --binary target/debug/rho-core --root /var/tmp/rho-core-platform-check
# 在外部完成已授权的整个内核停止/启动，再执行：
sudo python3 tests/linux_platform.py verify-reboot --binary target/debug/rho-core --root /var/tmp/rho-core-platform-check
```

默认宿主 UID 为 501、拒绝连接的 UID 为 65534；其他环境用 `--uid` 与 `--other-uid` 指定现有用户。
每项首次执行要求对应夹具子目录不存在。脚本保存原回复、上下文、二进制摘要与派发计数；
`prepare-reboot` 只杀夹具宿主，故意保留静默任务，脚本自身不重启系统。
`verify-reboot` 要求内核标识改变、启动时间改变且当前运行时长符合准备后发生的启动，
再检验原身份、释放与删除事实；不要求任意两次观察的运行时长都降低。
macOS 跨 UID 检查与独立 VM 故障实验见 [`tests/macos_platform.py`](tests/macos_platform.py)
和 [故障验证说明](tests/independent_vm.md)。

Windows（`x86_64-pc-windows-msvc`）在本机直接运行以上 Cargo 命令验证。平台语义的
映射与差异见 [Windows 支持](docs/WINDOWS.md)。

按用户决定（2026-10-10）暂不配置 CI；提交前在本地依次运行以上命令。各平台实际验证过
的范围见 [进度](docs/PROGRESS.md#已验证的范围)。

## 旧实现

Core 于 2026-10-09 起从零重建。原实现、测试与设计完整保存在分支
`codex/legacy-core-before-rebuild`，只在具体需求或故障需要对照时查阅；旧 API、测试、
SDK 与存储格式都不是新 Core 的要求，归档中旧测试的通过也不是新 Core 的验收：

```sh
git ls-tree --name-only codex/legacy-core-before-rebuild
git show codex/legacy-core-before-rebuild:Cargo.toml
```

磁盘上可能残留被忽略的旧 `crates/`、`sdk/` 或混有旧缓存的 `target/`，它们不是新源码、
新构建或新验收的证据。
