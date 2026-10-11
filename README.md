# Rho Core

Rho Core 为 Agent 工作环境补足需要管理的本地执行：定位选定对象、持有持续动作、
保护约定范围的重复送达，并找回结果。Agent 在自己的环境中阅读文档、编写与组合
代码、直接使用已有 CLI、SDK 和 MCP；只有存在明确执行缺口的动作才进入 Core。
模型循环、协议兼容与科研方法由各自的 Owner 承担。

本仓库已选择从零重建。`main` 包含设计与开发规则，以及首个受管流程的 Rust 库
`rho-core`：仅支持 Unix；受理与独立执行事实使用 SQLite 事务持久保存，脚本和输出为内容寻址文件，
重启后可按调用方列出原操作且不重派发，前一实例留下的运行经同一身份上下文的进程表或两个不同的
内核启动标识确认已不在后可删除。局部启动标识变化保留未知与删除保护。`rho-core` 二进制提供本地常驻宿主与命令行客户端，让 Agent 的多次独立 shell 调用
使用同一组受管运行；尚无公共 SDK、网络或 MCP 接入。原实现、原测试和原设计完整保存在分支
`codex/legacy-core-before-rebuild`。新源码在 `main` 分阶段建立，设计文档继续留在 `main`。

| 入口 | 内容 |
| --- | --- |
| [信息模型与存储不变量](docs/INFORMATION-MODEL.md) | 请求键、操作身份、独立事实、事务边界、单调更新与删除保护 |
| [受管运行契约](docs/MANAGED-RUN.md) | 调用方法、身份与去重、持有与停止、限额、存储与重启、不提供的保证 |
| [本地宿主与 CLI](docs/HOST-CLI.md) | Agent 跨调用提交、等待、读输出与取消；退出码、宿主限额与故障语义 |
| [Claude Code 行为评估](evals/claude-code/README.md) | 指定 Magpie/WestlakeHPC 模型使用 Core 的独立评估；真实夹具、事实评分与重复试验 |
| [可独立核验的分析试点](examples/scientific-pilot/README.md) | 真实 Iris 精确置换检验、独立数学 oracle、公开 CLI 接续与取消 |
| [四方向计划与本轮记录](docs/work/CORE-20261011-04.md) | 评估可靠性、真实 CLI Agent、可核验分析与平台故障验证 |
| [使命与实施计划](docs/MISSION-AND-PLAN.md) | Agent 工作方式、受管价值、按需里程碑与验收 |
| [目标架构](docs/ARCHITECTURE.md) | 核心职责的做什么、为什么、如何做到；依赖方向与执行保证 |
| [工程实践指南](docs/ENGINEERING.md) | 首个流程的开工条件、必要信息、故障验证、回归与交付闭环 |
| [工程进度总览](docs/PROGRESS.md) | 当前任务、近期交付、下一步与详细记录入口 |
| [工程进度与耗时规则](docs/ENGINEERING-MONITORING.md) | 做了什么、什么时候做、用了多久；记录模板与时间口径 |
| [审核报告取舍](docs/AUDIT-DECISIONS.md) | 本轮设计问题、纠正决定、核对来源与最小验证 |
| [用户原始审核报告](docs/review/USER-REPORT.md) | 保留的审核输入，包含尚未核实的主张 |
| [重建与分支规则](docs/REBUILD.md) | 归档、主分支开发、独立验收与后续集成 |
| [工作约束](AGENTS.md) | 后续实施时遵循的规则 |

本轮只修改 Rho-core。[Rho 应用](https://github.com/YuLab-SMU/Rho) 和
[Rho-plugins](https://github.com/YuLab-SMU/Rho-plugins) 保持原状，它们当前的 SDK、
插件协议与组合选择不代表新 Core 的契约。协议接入层在 Core 的逻辑边界之外，
可以同仓同进程存在；不因此创建新仓库或要求全部外部工具编写 Rho 插件。
跨仓集成另行选定，旧应用的历史验证不能作为新 Core 的通过证据。

首个受管流程：普通程序在临时项目中调用一个原生执行夹具；结束等待后找到原执行
与结果，重复送达（含并发）不重派发，两个不同任务脚本无需增加 Core 方法入口。
调用与保证见 [受管运行契约](docs/MANAGED-RUN.md)，可运行调用见
[`examples/managed_run.rs`](examples/managed_run.rs)，行为回归见
[`tests/managed_run.rs`](tests/managed_run.rs)，故障实验见
[`tests/restart.rs`](tests/restart.rs)，存储失败与事实独立性见
[`tests/storage.rs`](tests/storage.rs)，宿主与 CLI 的跨进程回归见 [`tests/host.rs`](tests/host.rs)；各平台的实际验证范围见
[进度总览](docs/PROGRESS.md)。后续流程仍先复用实际工具确认缺口，不以重新建设文件
读取、统一工具目录或协议框架为起点。模型、协议和领域集成各自在真实消费者出现后
验证；没有观测服务。

许可证见 [LICENSE](LICENSE)。新实现的执行边界与数据处理要求分别见
[SECURITY](SECURITY.md) 和 [PRIVACY](PRIVACY.md)。

## 构建与检查

工具链由 [`rust-toolchain.toml`](rust-toolchain.toml) 固定。回归使用 `/bin/sh`、
`mkfifo` 与 `perl`，并在临时目录中创建和清理夹具进程。Cargo 命令依次运行，不并行：

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

macOS 与 OrbStack Ubuntu 25.04 均已依次通过格式、Clippy、完整 73 项 Cargo 回归与
`managed_run` 示例。Linux 的四个验证入口也已实跑：跨 UID 连接在权限放宽后的夹具中被宿主拒绝；
局部标识、来源范围与 PID 命名空间变化保留原任务与删除保护；整个 OrbStack 后端正常停止、启动后，
内核标识夹具得到 `Gone/BootChanged`，原身份仍只派发一次且可删除。局部标识夹具保持
`Detached/Protected`，不能证明消失。实验及证据见 [Linux 验证记录](docs/work/CORE-20261011-03.md)。
本轮另行通过 macOS 实际跨 UID 拒绝、独立 Debian guest 内核 panic 与虚拟断电后原身份、
输出保留及不重派检查；未验证 Mac 物理断电或真实存储硬件失电。本轮真实 Agent CLI 评估、
独立数学核验分析与故障证据见[四方向记录](docs/work/CORE-20261011-04.md)。

按用户决定（2026-10-10）暂不配置 CI，也不做多平台 CI；提交前在本地依次运行以上
命令，实际验证过的平台见 [进度总览](docs/PROGRESS.md)。

Agent 评估按已选范围独立运行，使用 Claude Code 和 Magpie 的
`westlakehpc/deepseek-v4.1-flash`，不成为普通 Cargo 检查或 Core 生产依赖。
运行入口及未评分范围见 [评估说明](evals/claude-code/README.md)。
