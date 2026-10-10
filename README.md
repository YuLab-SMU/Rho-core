# Rho Core

Rho Core 为研究者与外部 Agent 提供可直接调用的本地科学工具运行底座：取得真实材料、
执行明确动作、读取结果，并在工作中断后找到继续所需的信息。模型、提示、计划、
对话记忆与 Agent 编排由调用方承担；科研意义由研究者及领域工具判断。

本仓库已选择从零重建。`main` 当前是设计与开发规则基线，尚无新运行时、公共 SDK 或
可执行构建。原实现、原测试和原设计完整保存在本地分支
`codex/legacy-core-before-rebuild`。新源码在 `main` 分阶段建立，设计文档继续留在 `main`。

| 入口 | 内容 |
| --- | --- |
| [使命与实施计划](docs/MISSION-AND-PLAN.md) | 用户动作、最小范围、里程碑与完成条件 |
| [目标架构](docs/ARCHITECTURE.md) | 最小调用路径、读写与执行边界、必要记录 |
| [审核报告取舍](docs/AUDIT-DECISIONS.md) | 采用哪些建议、纠正哪些推论、如何验证 |
| [用户原始审核报告](docs/review/USER-REPORT.md) | 保留的审核输入，包含尚未核实的主张 |
| [重建与分支规则](docs/REBUILD.md) | 归档、主分支开发、独立验收与后续集成 |
| [工作约束](AGENTS.md) | 后续实施时遵循的规则 |

本轮只整理 Rho-core。[Rho 应用](https://github.com/YuLab-SMU/Rho) 和
[Rho-plugins](https://github.com/YuLab-SMU/Rho-plugins) 保持原状，它们当前的 SDK、
插件协议与组合选择不代表新 Core 的契约。跨仓集成将在选定的新契约通过独立验收后
另行开展。旧应用的状态与历史验证不能作为新 Core 的通过证据。

首个实施目标：在临时项目中，通过一个公开入口调用协议夹具，读取真实文件并返回
有界结果；读取不需要窗口、模型、权限配置向导或查询持久化。详细条件见实施计划。

许可证见 [LICENSE](LICENSE)。新实现的执行边界与数据处理要求分别见
[SECURITY](SECURITY.md) 和 [PRIVACY](PRIVACY.md)。
