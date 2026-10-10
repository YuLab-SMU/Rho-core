# Managed Operation 信息模型与存储不变量

本模型只描述 Core 实际受理的本地进程任务。调用方式、资源限额、重启范围与不提供的
保证见 [Managed run](MANAGED-RUN.md)；类型维护于 [record.rs](../src/record.rs)。
历史任务或外部工具的当前产物不能补造 Core 受理。

## 身份与原请求

| 字段 | 含义与范围 |
| --- | --- |
| `scope_id` | 存储初次创建时保存的范围身份，绑定规范化项目根；不使用 PID 或调用方临时连接 |
| `caller_id` | 可信单用户环境中的调用方命名空间，不是认证 |
| `request_id` | 调用方代持的重复送达键；同键不同原请求拒绝 |
| `operation_id` | 一次受理的执行关联，保存在行中；重启不重新生成，显式删除后再受理才产生新身份 |
| `request` | 不可变 `RequestInfo`，包含原工作目录、解析目录、执行方、实际参数、代码摘要及字节数 |
| `script` | 原代码的不可变 Blob 引用；执行前验证摘要与长度；空脚本也保留引用 |
| `accepted_at` | 受理时刻；存在受理不表示原生效果已发生 |

SQLite 对 `(scope_id, caller_id, request_id)` 加唯一约束；代码、目录、执行方名称与参数的
带长度 SHA-256 指纹仍用于原请求冲突判定。既有受理的重复送达先于新的对象/容量检查。

## 独立事实

`RunView.operation` 返回 `OperationRecord`。`Knowledge<T>` 为
`Known { value, observed_at, source }` 或 `Unknown { reason, since }`。
时间是 Core 的观察时间，来源为 Core、原生进程、存储恢复或运行锁。各维度不能相互替代。

| 维度 | 可确认的内容 | 不可推导的内容 |
| --- | --- | --- |
| Dispatch | 初始 `NotAttempted`；调用执行方之前持久写入 `Attempted` | `Attempted` 不保证启动成功或原生效果 |
| Execution | 明确 `NotStarted`、`SpawnObserved`、`ExitObserved`；保留已知 PID、启动观测与退出结果 | 历史启动观测不是当前存活，失去持有不抹掉它；退出码未知不表示未执行 |
| Cancellation | 请求原因/时间、系统调用成功的 TERM/KILL 时间与发送错误 | 请求或发信号不等于退出、组释放或回滚 |
| Outputs | 每路的保留/观察字节、限制、EOF、错误、观测来源与时间、不可变片段引用 | 文件存在不等于元数据已提交；输出失败不改变原生退出 |
| Group release | 在实际持有边界观察到是否还有组成员 | 退出码、运行锁状态均不能代替组释放确认 |
| Run lock | 当前观察到原锁是否被持有，或明确观察错误；不会保存成原生退出 | 任务可关闭 stdin；锁释放不证明结束，也不授权删除 |
| Retention | 依据已知完成/释放、当前持有与未保存事实，投影 Protected 或 Removable | 单个便利状态 `Finished` 不足以删除；不自动过期或淘汰 |
| Revision | 已提交事实的版本；每个写入比较当前版本并递增 | 未提交内存观察不冒充持久版本；陈旧更新拒绝 |

`RunView.status` 仅为调用便利投影，不是存储状态机。`held` 在主进程退出但还排空输出时
仍为真。完整停止、组释放、输出保存和当前可删除分别判断。未知执行持续占用请求键。
`list_operations(caller)` 找回该调用方全部保留操作，范围受存储记录容量约束。

## 存储与事务边界

`OperationStore` 是当前流程的一种具体 SQLite 实现，不预建多后端接口、调度器或恢复引擎。

- `store_info` 保存格式版本、项目根字节和 scope；已有空/损坏/其他格式数据库不初始化成空库。
- `operations` 保存不可变受理和每个独立事实；`artifacts` 保存脚本及每路输出的摘要、长度和连续偏移。
- `BEGIN IMMEDIATE` 内完成幂等键与全局记录容量检查、插入受理；COMMIT 成功之后才能派发。
- 派发标记先单独 COMMIT，再调用原生执行方；该窗口可能只有标记而没有进程，未知也不得重派。
- 启动/退出、取消、两路输出、释放各有事务。输出字节在 Blob 同步后才能被事务引用。
  中断的元数据更新不发布部分输出；已有已提交输出不因退出码缺失而丢失。
- 读取损坏行/不一致引用报错，保留请求键和材料。缓存不掩盖持久损坏；查库失败不是 NotFound。
- 新受理和清理核对现有行身份，无法可靠解释的行不能让键被当作空闲，也不能触发材料清理。
- 显式删除在事务内再次核对 operation_id、revision 和完成/释放事实；未提交删除不释放键。
  只有提交后才能再次受理同键。清理仅移除无数据库引用且无待保存内存引用的 Blob。

SQLite 配置为 WAL、FULL 同步，并启用 macOS 的 fullfsync。当前锁定的 rusqlite 0.40.2
包含 SQLite 3.53.2，已包含 [WAL-reset 损坏修复](https://www.sqlite.org/wal.html#walreset)。
[官方 WAL 文档](https://www.sqlite.org/wal.html) 与
[synchronous 说明](https://www.sqlite.org/pragma.html#pragma_synchronous) 定义数据库保证；
数据库事务不包含原生副作用，也不恢复已经丢失的原生句柄。物理断电仍需单独实验。
旧文件格式明确拒绝，原文件保留；本次不引入自动迁移或双写。

## 单调更新与可验证行为

已知观察不得退为 Unknown 或更早观察。已观察退出不得退回启动，终结事实不得改变原生身份；
已有输出偏移、已保留片段、观察计数和 EOF 不得回退。信号成功时间与原取消意图不能删除或替换。
乐观版本检查拒绝旧写入，独立维度仍可分别保存。

可重试的只是保存同一已知事实，不是执行原任务。没有持久派发尝试时，重启可以确认没有
调用执行方；有派发尝试但没有结束观测时保留未知。当前没有基于租约到期的夺回或自动派发。

验收使用实际 `/bin/sh` 原生效果、输出字节、并发重复送达、宿主 SIGKILL、SQLite FULL/只读
错误与定向事务失败。回归入口为 [managed_run](../tests/managed_run.rs)、
[restart](../tests/restart.rs)、[storage](../tests/storage.rs) 与存储单元检查。
实际平台、失败依据和未验证范围见 [Progress](PROGRESS.md)，不将 SQLite 配置替代故障实验。
