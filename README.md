# Rho Core

Rho Core 为 Agent 工作环境补足需要管理的本地执行：定位选定对象、持有持续动作、
保护约定范围的重复送达，并找回结果。Agent 在自己的环境中阅读文档、编写与组合
代码、直接使用已有 CLI、SDK 和 MCP；只有存在明确执行缺口的动作才进入 Core。
模型循环、协议兼容与科研方法由各自的 Owner 承担。

本仓库已选择从零重建。`main` 包含设计与开发规则，以及首个受管流程的 Rust 库
`rho-core`：仅支持 Unix；受理记录持久保存在 `state_dir`，重启后可找回且不重派发
（阶段 2）；尚无公共 SDK 或协议接入。原实现、原测试和原设计完整保存在分支
`codex/legacy-core-before-rebuild`。新源码在 `main` 分阶段建立，设计文档继续留在 `main`。

| 入口 | 内容 |
| --- | --- |
| [受管运行契约](docs/MANAGED-RUN.md) | 调用方法、身份与去重、持有与停止、限额、存储与重启、不提供的保证 |
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
[`tests/restart.rs`](tests/restart.rs)；各平台的实际验证范围见
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

按用户决定（2026-10-10）暂不配置 CI，也不做多平台 CI；提交前在本地依次运行以上
命令，实际验证过的平台见 [进度总览](docs/PROGRESS.md)。
