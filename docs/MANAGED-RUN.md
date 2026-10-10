# 受管运行：调用契约

本文是 `rho-core` 首个受管流程的契约与能力说明，也是它的唯一维护源：
`rho_core::CONTRACT` 在运行时返回本文全文，`Core::capability()` 同时给出本实例的
项目根、执行方、实际限额与运行文件位置。可运行调用见
[`examples/managed_run.rs`](../examples/managed_run.rs)，行为回归见
[`tests/managed_run.rs`](../tests/managed_run.rs)，跨重启与故障实验见
[`tests/restart.rs`](../tests/restart.rs)。受理与独立事实保存在 `state_dir/core.sqlite3`，脚本与输出存于不可变 Blob：之后在同一
目录上打开的 `Core` 找得到原记录，不会再次派发。信息模型见
[Information model](INFORMATION-MODEL.md)；前一实例的原生任务不会被重新持有。

## 何时使用

已有 shell 或工具能满足需要时直接使用它们。只有一段代码需要以下保证时才交给 Core：

- 调用方持有请求身份，重复送达（含并发）只得到一次受理与一次原生派发；
- 运行由 Core 持有，与某个等待者无关；等待者离开后，另一个等待者用同一身份找回原执行；
- 原请求、执行事实与有界保留的输出可以按需读取；
- 停止请求与停止确认分别表达，进程组的释放可检查。

对照实测：直接用 shell 运行同一意图两次会派发两次；后台进程在等待者退出后成为
孤儿（父进程变为 1，且不在独立进程组），只剩进程号可找，没有请求关联和输出入口。

## 调用

```rust
use std::time::Duration;
use rho_core::{Core, CoreConfig, Executor, RunRequest, Stream};

let core = Core::open(
    CoreConfig::new("/path/to/project", "/path/to/state")
        .executor(Executor::new("sh", "/bin/sh")),
)?;
let submitted = core.submit("agent-a", RunRequest {
    request_id: "sum-100".into(),
    executor: "sh".into(),
    workdir: "analysis".into(),
    code: "seq 1 \"$1\" | awk '{ s += $1 } END { print s }'".into(),
    args: vec!["100".into()],
})?;
let view = core.wait("agent-a", "sum-100", Duration::from_secs(30))?;
let chunk = core.read_output("agent-a", "sum-100", Stream::Stdout, 0, 64 * 1024)?;
```

| 方法 | 作用 |
| --- | --- |
| `Core::open(config)` | 规范化项目根，检查执行方与限额，独占打开 `state_dir` 并载入已有记录 |
| `submit(caller, request)` | 受理并启动，或返回该身份已有的受理；`Disposition::Accepted` 或 `Existing` |
| `lookup(caller, request_id)` | 立即返回已知事实；损坏与存储不可用分别报错 |
| `list_operations(caller)` | 列出该调用方的保留操作，重启后无需记住所有 request_id；同样明确报告读取错误 |
| `wait(caller, request_id, timeout)` | 最多等待 `timeout` 直到终态，返回当时的事实；超时只结束这个等待者 |
| `read_output(caller, request_id, stream, offset, max_bytes)` | 从 `offset` 读取已保留输出，单次最多 `min(max_bytes, max_read_bytes)` 字节 |
| `cancel(caller, request_id)` | 记录停止请求并开始发信号；确认以终态为准 |
| `shutdown()` | 拒绝新受理，请求停止本实例持有中的运行并等待确认 |
| `forget(caller, request_id)` | 仅删除已确认结束、释放且事实已保存的记录；删除事务提交后身份才不再去重 |
| `unreadable_records()` | 读不出的数据库行及其身份、原因，或数据库查询错误 |
| `capability()` | 本文、项目根、执行方、实际限额与 `state_dir` |

`Core` 可在线程间共享（引用或 `Arc`）。

## 请求、身份与去重

- **执行方**由应用在 `CoreConfig` 中选定：名称、绝对路径程序和前置参数，不做 `PATH`
  查找。请求只能按名称选择已配置的执行方。
- **实际执行**：`<program> <执行方参数...> <代码快照路径> <请求参数...>`，工作目录为
  解析后的 `workdir`，标准输入为空文件（读到即结束），标准输出与标准错误由 Core 读取，环境变量继承
  Core 所在进程。`code` 在受理事务提交前保存为内容寻址快照，启动前验证长度与摘要，执行的正是该快照。
- **身份**：`caller` 与 `request_id` 均为 1–128 个 `[A-Za-z0-9._:@-]` 字符。身份的
  范围是“`state_dir` 绑定的项目 × `caller`”，跨 `Core` 实例有效。可信单用户环境中 `caller` 是命名空间，
  不是认证。
- **同一请求**：`executor`、`workdir`（按给出的字节，`work` 与 `./work` 不同）、`code`
  与 `args` 逐字节相同，以带长度前缀的 SHA-256 指纹比较。
  - 同身份同请求：返回原受理（`Existing`），不再派发，无论原运行处于何种状态；
  - 同身份不同请求：`SubmitError::Conflict`，附带原记录；原记录不变，不派发；
  - 有意再执行：使用新的 `request_id`。
- **并发**：同一身份的检查、全局记录容量检查与插入在同一个 SQLite `BEGIN IMMEDIATE` 事务中完成，
  唯一键为 `(scope_id, caller_id, request_id)`。进程内同时预留运行槽，并发提交中只有一个得到 `Accepted`。
  重复送达优先于其他检查：原请求被受理后，即使当前目录已不存在、实例正在关闭或
  已经重启，同身份同请求仍返回原受理。

## 对象与范围检查

在受理之前检查；被拒绝的请求不建立记录，也不启动进程。

| 拒绝 | 条件 |
| --- | --- |
| `InvalidRequest` | 身份格式不符；代码、参数个数或参数总字节超限；参数含 NUL |
| `UnknownExecutor` | 执行方名称未配置 |
| `Workdir { NotRelative }` | 空路径或绝对路径；`workdir` 必须相对于项目根 |
| `Workdir { NotFound / NotADirectory / Unreadable }` | 解析失败或不是目录 |
| `Workdir { OutsideProject }` | 规范化（含符号链接）后位于项目根之外 |
| `Capacity` / `ShuttingDown` | 见资源与关闭 |
| `RecordUnreadable` | 该身份的记录存在但读不出；它可能已经运行过，因此不派发 |
| `Storage` | 存储事务或查询失败。受理或派发标记提交失败时不调用执行方；派发后的查询也可能失败，错误不能证明没有执行。提交结果不确定时可能已有受理，下一次读取/提交必须重新核对持久记录 |

这是包含性检查，不是沙箱：任务以用户的 OS 权限运行，可以访问 OS 允许的任何路径；
检查与进程启动之间目录仍可能被替换。读取、等待、取消与读输出只在同一 `caller`
范围内可见；另一 `caller` 使用相同 `request_id` 是独立请求。

## 可以取得的事实

`RunView` 由 `submit`、`lookup`、`wait`、`cancel` 返回：

| 字段 | 内容 |
| --- | --- |
| `run_id` | 执行身份，含受理它的实例标记；进程号不是身份 |
| `request` | 原请求：`caller`、`request_id`、执行方名称与程序、`workdir` 及其解析结果、代码 SHA-256 与字节数、实际参数 |
| `accepted_at` | 受理时刻 |
| `operation` | 原请求、请求键、脚本引用，以及带来源与时刻的独立派发/执行事实、输出、取消、释放、保留保护与持久 revision |
| `status` | 原有 `Starting`、`NotStarted`、`Running`、`Finished`、`Detached` 的便利投影；存储不保存这个组合状态 |
| `held` | 当前实例仍在启动、持有进程或排空输出；主进程退出与持有结束不同 |
| `cancel` | 停止请求的原因与时刻，以及实际发出 SIGTERM、SIGKILL 的时刻 |
| `stdout` / `stderr` | 已保留字节、已读到字节、上限、是否读到流结束、保留错误 |
| `unsaved` | 已知但尚未写入存储的事实；之后读取时重试保存，在此之前重启看不到它 |

- 时间均为 Core 的观察时刻，不读取原生进程启动时间。`Knowledge::Known` 附带
  `observed_at` 和 `source`；`Unknown` 附带原因与起点。历史启动观测不等于现在仍运行。
- `Finished` 可以在主进程退出后、输出排空之前出现；`RunView::is_terminal()` 还要求
  `held == false`。`Detached` 不会因运行锁释放变成终态。能否删除应检查
  `operation.retention`，不能只检查 `status` 或 `is_terminal()`。
- 便利字段 `group_released == false` 包括未确认；精确信息在 `operation.group_released`。
  `Detached.present` 只投影运行锁是否被持有；观察失败的原因在 `operation.run_lock`。
- `pid` 只在 `Finished` 之前指向该进程；回收后可能被系统复用，Core 不再向它发信号。
- `NotStarted` 表示已受理且已知没有启动进程：准备运行文件失败、程序无法启动或启动前
  已请求停止。重复送达返回同一记录，不会重试。
- `Exit::Unknown` 表示 Core 已不再持有进程，但读不到退出状态（例如宿主进程把 SIGCHLD
  设为忽略，导致子进程被自动回收）。
- `truncated()` 为真表示有字节超过保留上限，已读到但未保留；`retention_error` 表示保留
  写入或管道读取失败，此后的字节没有保留。这些都不改变进程的退出事实。
- `LookupError::NotFound` 只在数据库成功查询且无该身份受理时返回，不证明别处没有运行。
  受理存在但字段损坏返回 `RecordUnreadable`，查询不可用返回 `Storage`；已有内存缓存
  也不能掩盖持久记录损坏。输出 Blob 缺失、损坏或读失败返回 `ReadError::Io`，不改写退出事实。

## 持有、停止与释放

- 每次运行在独立进程组中启动（组号等于主进程号），由 Core 的一个持有线程负责读取
  输出、发信号与回收；只有该线程发信号，且只在回收之前发，因此不会误伤复用的进程号。
  回收之后只用空信号检查组内是否仍有成员；组号若已被复用，结果偏向报告未释放。
- 等待者超时或线程结束不影响运行。
- **主进程退出**：Core 先在不回收的情况下观察到退出，此时组号仍被占用，向整个进程组
  发 SIGKILL 结束剩余成员，再回收主进程，随后检查组内是否已无成员（`group_released`）。
  因此任务留下的后台成员不会比主进程活得更久。用 `setsid` 等方式离开进程组的进程
  不在 Core 持有范围内。
- **输出关闭**：主进程退出后最多等待 `output_close_grace`，直到两路输出都读到结束且
  进程组为空。仍未关闭时停止读取，`eof` 为假，之后写入的字节不保留。
- **取消**：`cancel` 记录请求；持有线程向进程组发 SIGTERM，超过 `stop_grace` 仍未退出时
  发 SIGKILL。返回值只说明请求已提出，保存失败在 `unsaved` 中报告；信号系统调用成功才填
  `term_sent_at` / `kill_sent_at`，失败在 `signal_error` 中报告。停止以真实执行观测确认，
  释放以 `operation.group_released` 的已知值确认。对已结束的运行取消不产生变化。取消不回滚任务已写入的文件或其他效果。
- **关闭**：`shutdown()` 拒绝新身份（已有身份仍可找回），对未结束运行按上述方式请求停止，
  最多等待 `stop_grace + output_close_grace + 1s`，报告已确认停止与未确认的运行。
  `Core` 被 drop 时执行同样的关闭，记录保留。未持有的运行不发信号、不等待，取消也不记录：本实例无法对它采取动作。
  存储损坏不会阻止关闭当前实例确实持有的原生资源，保存失败仍如实报告。

## 资源与超限

| 资源（`Limits` 字段） | 默认值 | 超限行为 |
| --- | --- | --- |
| 同时持有的进程 `max_running` | 4 | 新身份被拒绝（`Capacity`），不排队；重复送达不受影响 |
| `state_dir` 中的记录 `max_records` | 256 | 计入之前实例与读不出的记录；新身份被拒绝；从不淘汰，只能用 `forget` 删除 |
| 代码 `max_code_bytes` | 1 MiB | `InvalidRequest` |
| 参数个数 `max_args`、总字节 `max_arg_bytes` | 256、64 KiB | `InvalidRequest` |
| 每路输出保留 `max_stream_bytes` | 1 MiB | 超出部分读取后丢弃并计数，`truncated()` 为真 |
| 每路输出片段 `max_output_chunks` | 4096 | 保护 Blob 引用、数据库行与内存；耗尽后记录 `retention_error`，后续输出继续读取计数但不保留 |
| 单次读取 `max_read_bytes` | 64 KiB | 返回较短片段，按 `offset` 续读；越过已保留字节返回 `OffsetBeyondRetained` |
| 停止宽限 `stop_grace` | 2 s | 之后发 SIGKILL |
| 输出关闭宽限 `output_close_grace` | 1 s | 之后停止读取并如实标记 |

每个运行占用一个持有线程。磁盘占用上限约为 `max_records × (代码 + 2 × max_stream_bytes)`。
每个输出 Blob 最多 64 KiB；片段数量上限还保护元数据。数据库页和索引占用额外空间，
删除后 SQLite 可复用空闲页，物理数据库不保证缩小。未引用 Blob 在打开、下一次受理与
删除后的清理中移除；未补存输出的 Blob 被内存引用保护，不能被另一个记录的删除清理。
Core 不限制任务自身写入项目的文件、网络或其他资源，这些属于实际执行环境。

## 保留期限与存储

- 一个 `state_dir` 同一时刻只由一个 `Core` 使用，绑定第一次打开时的项目根。独占锁由
  Core 及其持有线程保留，任务只继承自己的运行锁，不持有 SQLite 连接。
- `core.sqlite3` 保存不可变受理、独立事实、revision 与输出引用；`blobs/sha256/<前两位>/<摘要>`
  保存脚本和不可变输出片段；`runs/<operation_id>/run.lock` 仅用于锁观察，不是事实数据库。
  数据文件权限为 0600，新建目录为 0700；内容可能含私有数据或凭据，不做脱敏。
- SQLite 使用 `WAL`、`synchronous=FULL` 与 `fullfsync=ON`；Blob 在引用提交前同步。
  已提交事务与未提交事务由 SQLite 恢复区分，未提交修改不会成为已受理或已保留事实。
  配置依据见 [SQLite synchronous](https://www.sqlite.org/pragma.html#pragma_synchronous)
  与 [fullfsync](https://www.sqlite.org/pragma.html#pragma_fullfsync)。本次实验验证宿主 SIGKILL、
  SQLite 容量与只读错误；没有断电或 OS 崩溃实验，不能据此宣称这些已经验证。
- **受理**：幂等键检查、所有存储记录的容量检查与新记录插入在一个写事务中完成。
  成功提交后才允许派发；同一键始终找回原受理。派发调用前单独提交派发标记。
- **事实保存**：启动/退出观测、取消、stdout、stderr 和释放各自独立提交；每次用 revision
  拒绝陈旧写入。失败事实保留在内存，`unsaved` 明确说明，之后读取只重试补存，不重跑原生任务。
  输出 Blob 保留失败仅影响相应输出；输出元数据失败不能伪造或阻止已知退出的独立保存。
- **重启**：

  | 已保存事实 | 重开后的结论 |
  | --- | --- |
  | 已受理，派发明确为 `NotAttempted` | 保存 `NotStarted` 结论，不派发 |
  | 有派发标记，无启动或退出观测 | 启动未知，保留受理与已有输出，不派发 |
  | 有启动观测，无退出观测 | 保留原启动观测；现在的退出未知，不派发 |
  | 有退出观测，无释放确认 | 保留原退出，释放继续未知，禁止删除 |
  | 有退出与释放确认 | 原事实与有界输出可读；没有未保存事实时允许显式删除 |

  运行锁是否仍被持有只是一项附带当前观察。任务可以关闭/替换标准输入，或离开原进程组；
  锁未持有不证明任务结束，观察失败也不是不存在。之前实例的未知记录持续受保护，
  本版不提供重新持有、发信号或解除其不确定保护的入口。之后输出无法继续保存，
  且任务写失去读端的管道可能收到 SIGPIPE。
- **损坏**：数据库损坏/格式不符时 `open` 失败。行字段或引用不一致时明确报错，保留
  幂等键和材料；新受理也不能绕过损坏身份。
- **开发数据**：当前仅维护 SQLite 格式。开发阶段变更存储格式时直接清除已停用的旧测试状态，
  不建立旧格式识别、保留或迁移机制。
- **保留与删除**：不过期、不自动淘汰。`forget` 要求已知未启动或已观察结束、已确认组释放，
  并且本实例不再持有、没有待保存事实。删除操作与引用的事务提交后才释放键；中断前
  事务回滚，仍返回原受理。之后同键再次提交是新的受理与执行。Blob 清理不撤销已提交删除；
  逻辑删除不承诺介质上的安全擦除。
- **备份**：在线元数据备份使用 SQLite 的 backup API；它不是包含 Blob 的完整运行备份。
  完整副本须在无 Core 写入时保存数据库与对应不可变 Blob。数据库处于 WAL 模式时，
  不可单独复制主库或手工删除 WAL；SHM 可由 SQLite 重建，不代表持久提交。
  见 [SQLite WAL](https://www.sqlite.org/wal.html#the_wal_file)。恢复副本不恢复原生句柄、
  活进程或运行锁的内核状态。

## 不提供的保证

不在重启后重新持有、停止或回收前一实例的进程，不补记它未保存的退出状态；不提供
外部 exactly-once、多个运行之间的事务、回滚、自动重试，
也不登记 Core 之外的进程或文件修改。工作目录检查不是隔离。平台仅限 Unix；实际验证过的
平台见 [进度总览](PROGRESS.md)。
