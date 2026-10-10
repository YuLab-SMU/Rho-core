# 受管运行：阶段 1 调用契约

本文是 `rho-core` 首个受管流程的契约与能力说明，也是它的唯一维护源：
`rho_core::CONTRACT` 在运行时返回本文全文，`Core::capability()` 同时给出本实例的
项目根、执行方、实际限额与运行文件位置。可运行调用见
[`examples/managed_run.rs`](../examples/managed_run.rs)，行为回归见
[`tests/managed_run.rs`](../tests/managed_run.rs)。全部保证只在一个 `Core` 值的
寿命内成立；跨重启持久化属于阶段 2，尚未实现。

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
| `Core::open(config)` | 规范化项目根，检查执行方与限额，在 `state_dir` 下新建本实例目录 |
| `submit(caller, request)` | 受理并启动，或返回该身份已有的受理；`Disposition::Accepted` 或 `Existing` |
| `lookup(caller, request_id)` | 立即返回已知事实 |
| `wait(caller, request_id, timeout)` | 最多等待 `timeout` 直到终态，返回当时的事实；超时只结束这个等待者 |
| `read_output(caller, request_id, stream, offset, max_bytes)` | 从 `offset` 读取已保留输出，单次最多 `min(max_bytes, max_read_bytes)` 字节 |
| `cancel(caller, request_id)` | 记录停止请求并开始发信号；确认以终态为准 |
| `shutdown()` | 拒绝新受理，请求停止全部持有中的运行并等待确认 |
| `capability()` | 本文、项目根、执行方、实际限额与本实例运行目录 |

`Core` 可在线程间共享（引用或 `Arc`）。

## 请求、身份与去重

- **执行方**由应用在 `CoreConfig` 中选定：名称、绝对路径程序和前置参数，不做 `PATH`
  查找。请求只能按名称选择已配置的执行方。
- **实际执行**：`<program> <执行方参数...> <代码快照路径> <请求参数...>`，工作目录为
  解析后的 `workdir`，标准输入为空，标准输出与标准错误由 Core 读取，环境变量继承
  Core 所在进程。`code` 在受理后写入本次运行的快照文件，执行的正是该快照。
- **身份**：`caller` 与 `request_id` 均为 1–128 个 `[A-Za-z0-9._:@-]` 字符。身份的
  范围是“本 `Core` 实例的项目 × `caller`”。可信单用户环境中 `caller` 是命名空间，
  不是认证。
- **同一请求**：`executor`、`workdir`（按给出的字节，`work` 与 `./work` 不同）、`code`
  与 `args` 逐字节相同，以带长度前缀的 SHA-256 指纹比较。
  - 同身份同请求：返回原受理（`Existing`），不再派发，无论原运行处于何种状态；
  - 同身份不同请求：`SubmitError::Conflict`，附带原记录；原记录不变，不派发；
  - 有意再执行：使用新的 `request_id`。
- **并发**：同一身份的受理决定在一个锁内作出，并发提交中只有一个得到 `Accepted`。
  重复送达优先于其他检查：原请求被受理后，即使当前目录已不存在或实例正在关闭，
  同身份同请求仍返回原受理。

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

这是包含性检查，不是沙箱：任务以用户的 OS 权限运行，可以访问 OS 允许的任何路径；
检查与进程启动之间目录仍可能被替换。读取、等待、取消与读输出只在同一 `caller`
范围内可见；另一 `caller` 使用相同 `request_id` 是独立请求。

## 可以取得的事实

`RunView` 由 `submit`、`lookup`、`wait`、`cancel` 返回：

| 字段 | 内容 |
| --- | --- |
| `run_id` | 本实例内唯一的执行身份；进程号不是身份 |
| `request` | 原请求：`caller`、`request_id`、执行方名称与程序、`workdir` 及其解析结果、代码 SHA-256 与字节数、实际参数 |
| `accepted_at` | 受理时刻 |
| `status` | `Starting`、`NotStarted { reason }`、`Running { pid, spawned_at }`、`Finished { pid, spawned_at, exit, exit_observed_at, group_released }` |
| `cancel` | 停止请求的原因与时刻，以及实际发出 SIGTERM、SIGKILL 的时刻 |
| `stdout` / `stderr` | 已保留字节、已读到字节、上限、是否读到流结束、保留错误 |

- 时间均为 Core 的观察时刻，不读取原生进程启动时间。
- `pid` 只在 `Finished` 之前指向该进程；回收后可能被系统复用，Core 不再向它发信号。
- `NotStarted` 表示已受理且已知没有启动进程：准备运行文件失败、程序无法启动或启动前
  已请求停止。重复送达返回同一记录，不会重试。
- `Exit::Unknown` 表示 Core 已不再持有进程，但读不到退出状态（例如宿主进程把 SIGCHLD
  设为忽略，导致子进程被自动回收）。
- `truncated()` 为真表示有字节超过保留上限，已读到但未保留；`retention_error` 表示保留
  写入或管道读取失败，此后的字节没有保留。这些都不改变进程的退出事实。
- `LookupError::NotFound` 只说明本实例在该范围内没有受理此身份，不证明别处没有运行。

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
  发 SIGKILL。返回值只说明请求已记录；停止以 `Finished` 确认，释放以 `group_released`
  确认。对已结束的运行取消不产生变化。取消不回滚任务已写入的文件或其他效果。
- **关闭**：`shutdown()` 拒绝新身份（已有身份仍可找回），对未结束运行按上述方式请求停止，
  最多等待 `stop_grace + output_close_grace + 1s`，报告已确认停止与未确认的运行。
  `Core` 被 drop 时执行同样的关闭；全部确认后删除本实例的运行目录。

## 资源与超限

| 资源（`Limits` 字段） | 默认值 | 超限行为 |
| --- | --- | --- |
| 同时持有的进程 `max_running` | 4 | 新身份被拒绝（`Capacity`），不排队；重复送达不受影响 |
| 本实例受理记录 `max_records` | 256 | 新身份被拒绝；从不淘汰旧记录 |
| 代码 `max_code_bytes` | 1 MiB | `InvalidRequest` |
| 参数个数 `max_args`、总字节 `max_arg_bytes` | 256、64 KiB | `InvalidRequest` |
| 每路输出保留 `max_stream_bytes` | 1 MiB | 超出部分读取后丢弃并计数，`truncated()` 为真 |
| 单次读取 `max_read_bytes` | 64 KiB | 返回较短片段，按 `offset` 续读；越过已保留字节返回 `OffsetBeyondRetained` |
| 停止宽限 `stop_grace` | 2 s | 之后发 SIGKILL |
| 输出关闭宽限 `output_close_grace` | 1 s | 之后停止读取并如实标记 |

每个运行占用一个持有线程。磁盘占用上限约为 `max_records × (代码 + 2 × max_stream_bytes)`。
Core 不限制任务自身写入项目的文件、网络或其他资源，这些属于实际执行环境。

## 保留期限与存储

- 记录、代码快照与保留输出只在 `Core` 值的寿命内存在，并且只在内存记录中被索引；
  没有删除或过期接口，寿命内不会出现身份过期后悄悄重新执行。
- 新的 `Core` 实例不认识之前实例的身份：再次提交会被当作新请求执行。需要跨进程寿命
  去重的调用方必须等待阶段 2，或自行保证不在新实例中重复提交。
- 运行文件位于 `state_dir/<实例>/<序号>/`：`script`、`stdout`、`stderr`，权限 0600。
  它们可能包含代码、私有数据或程序打印的凭据，不做脱敏。
- Core 所在进程崩溃时：运行中的任务在各自进程组中继续，不再被持有；实例目录残留；
  新实例不清理其他实例的目录，因为无法确认它们是否仍属于存活的进程。

## 不提供的保证

不提供跨重启去重或恢复、外部 exactly-once、多个运行之间的事务、回滚、自动重试，
也不登记 Core 之外的进程或文件修改。工作目录检查不是隔离。平台仅限 Unix；实际验证过的
平台见 [进度总览](PROGRESS.md)。
