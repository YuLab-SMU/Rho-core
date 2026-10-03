# Core 职责退出与能力保留要求

本文件规定 Core 收缩时必须保留的执行边界，以及退出能力的接收方。
这是职责交接要求；测试项目编排、源码分支和构建执行已从 Core 退出，
开发预览、场景、布局与可视模型也已退出，草稿切片仍待实施。
Core 的单仓交付可以先于消费端完成，产品能力是否恢复由接收方的真实流程验收决定。

## Core 继续拥有的责任

Core 保留项目和调用者身份、授权、Provider 路由、不可变包校验、实例生命周期、
通用资源访问，以及 Operation 的受理、幂等、取消、结算和原始日志。
视图连接和托管资源的安全边界保留；布局、画布、编辑内容及开发工作流不因此属于 Core。
插件通过公开契约接入，不读取 Core 私有数据库或另建执行账本。

查询仍是带范围、时间和完整性的观察。旧 Operation 为 `Uncertain` 时可以继续查询和
开展独立工作；匹配的当前状态不能证明旧动作的归因，也不能触发自动重放。

## 能力接收方

| 退出的职责 | 接收方及专属要求 | Core 保留的接口边界 |
| --- | --- | --- |
| 源码分支、检查点和构建 | [Studio](../../Rho-plugins/plugins/studio/MIGRATION.md) | 校验已有不可变产物，授权调用和记录 Operation |
| 开发预览和测试项目编排 | [Studio](../../Rho-plugins/plugins/studio/MIGRATION.md) 与外部测试工具 | 普通 Host、实例、资源与清理的公开机制 |
| 场景定义、编辑草稿、历史和准备工作流 | [Manager](../../Rho-plugins/plugins/manager/MIGRATION.md) | 每一步实际实例动作的授权、原请求和结果 |
| 布局、焦点和窗口恢复 | [Rho 应用外壳](../../Rho/docs/WORKBENCH-MIGRATION.md) | 连接认证、受限视图调用及实例安全边界 |
| 编辑草稿和内容冲突 | [Editor](../../Rho-plugins/plugins/editor/MIGRATION.md)、Studio | 有界资源和受管动作；真实文件操作仍归 Files |
| Console 输入和呈现状态 | [Console](../../Rho-plugins/plugins/console/MIGRATION.md) 与应用外壳 | 原 Operation 与 R Provider 的可定位身份 |
| 可视组件树和画布模型 | Studio，运行呈现由选定的 UI 消费端接入 | 有界通信、资源包含关系和授权 |

各接收方拥有自己的领域 Schema 和持久化语义。当前 Studio 和 Manager 尚无后端；
上述分配要求它们补齐能力实现，而不是只修改调用地址。暂不创建统一的替代插件。
如以后需要共享实现，接收方文档继续约束同一用户能力，抽包不能使其无人负责。

本表采用三仓相邻检出的相对链接。独立克隆时，接收方文件路径与仓库分别见
[插件仓库](https://github.com/YuLab-SMU/Rho-plugins) 和
[应用仓库](https://github.com/YuLab-SMU/Rho)。详细实施顺序由应用的
[Core 收缩计划](../../Rho/docs/CORE-SIMPLIFICATION.md)协调。

## 每次退出需要说明什么

实施者在该次变更中列出退出的公开能力、SDK 类型和专用存储，链接对应接收方要求，
并说明 Core 已证明的结果及接收方仍未完成的流程。能力不存在、Owner 未接入和
清理未确认分别表达；不得以一个成功状态掩盖这些差异。

Core 用最小公开调用方验收自己的新契约，不需要同时修复其他仓库。消费端的构建、
SDK 刷新、组合锁更新与用户流程验收是单独选择的任务。退出完成只能说明 Core 的
责任已收缩；接收方尚未验收时，相应产品流程仍标为待迁移。

## 全新项目与存储

本轮是破坏性升级。旧项目、旧目录和历史记录不再是保留、读取、导出或迁移要求；
此前要求保留旧内容的约束已取消。接收方仍负责同类用户能力，旧数据恢复不再是
Core 收缩的完成条件。验收使用新建临时项目和 catalog，不维护旧契约或恢复路径。

- 退出能力的 Schema、专用存储与历史访问入口直接删除，不藏在兼容开关后。
- 新契约下受理的 Operation 仍保存原身份、请求、结果和不确定性；不能自动重放。
- 关闭连接、关闭视图、停止测试 Host 和释放原生实例分别确认。失去观察不表示停止；
  不自动重启用户 Host、R 会话或未确认的开发任务。

## 已退出：测试项目编排

生产 Host 不再提供 `plugins.test_create/test_stop/test_project/test_projects/test_operation`，
也不管理子 Host、`plugin_test_projects` 表或 `test-projects-v1` 目录。
专用 SDK 类型、视图选择器、测试工作区导航和资产路由一并删除。
开发工具在自己创建的临时目录中直接启动普通 `session` 或 `mcp`，自行管理进程生命周期。
旧 `--test-project`、SessionFrame 选择字段、HTTP 选择头和视图选择字段被明确拒绝，
不会静默改为当前项目调用。接收方的开发 UI 和编排流程仍待实现。

## 已退出：源码分支与构建执行

Core 不再提供 `plugins.branch/advance_branch/checkpoint/build`，也不提供
`plugins.branches/branch_head/check_source`。CLI 的分支创建、推进与 head 查询一并删除。
可编辑分支表、分支起点、构建队列、`builds-v1` 目录管理和专属执行结算逻辑直接退出。
分支、编辑检查点与构建的 Rust 契约、SDK 类型和 Schema 随公开契约删除。

`plugins.source_tree/read_source/compare` 继续读取已有不可变包；源码读取仍验证整个
文件摘要，并按既有字节预算返回。包导入、导出、身份和产物校验保留，源声明中的构建
说明只作为元数据，不触发命令执行。实际编辑、Git 和构建由 Studio 或外部工具拥有，
接收方仍需实现自己的流程。

## 已退出：产品呈现模型

Core 不再提供 `plugins.preview`、`scenarios.*` 或 `windows.*`。预览专属实例类别、
固定查询夹具、场景版本与窗口布局表、可视组件树及其 JS 渲染器直接删除。
Rust、Schema 和 SDK 中的场景、布局与可视类型同步退出；包内 `views/*.json` 作为
Owner 自有字节保存与校验，Core 不再解释成预设画布格式。

普通 `views.open`、受限连接、资源和实例生命周期保留。窗口标识用于限制视图授权，
不再代表 Core 管理的布局。关闭仍不释放原生实例或取消其已受理的操作。
场景组合由 Manager 拥有，呈现与恢复由应用外壳和选定 UI 接入，接收方仍待迁移。

## Core 验收

普通 CLI 或 MCP 调用不要求窗口、场景、编辑器或 Studio 存在。固定 Provider、
项目和调用者隔离、机械授权、原生前置条件、受管执行和禁止重放继续成立。
退出测试项目编排后，外部测试调用方仍能在临时项目中运行、观察和明确结束普通 Host。
Core 不执行插件构建，也不把编辑内容、布局或整个项目的证据完整性作为普通调用门槛。

按实际改变的边界运行定向测试和契约检查。接收方的草稿恢复、真实构建、场景切换、
外部文件冲突和窗口行为由对应 Owner 验收，Core 夹具不代替这些结果。
