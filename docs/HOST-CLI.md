# 本地宿主与 CLI：Agent 接入

`rho-core` 二进制把[受管运行契约](MANAGED-RUN.md)交给以多次独立进程工作的调用方，
例如 Claude Code 的每次 Bash 调用。`rho-core serve` 是常驻宿主，为一个项目持有一个
`Core`；其余子命令是薄客户端，每次只向宿主发送一个请求并打印回复，本进程内不启动任何运行。
二进制只使用库的公共 API，Core 不依赖它，语义不变。实现见
[`src/bin/rho-core/`](../src/bin/rho-core/main.rs)，行为回归见 [`tests/host.rs`](../tests/host.rs)。

## 为什么需要常驻宿主

2026-10-11 在 macOS 上实测两种直接做法：

- 每次调用一个持有 Core 的进程：提交一个持续任务后进程退出，`Core` 在 drop 时关闭并停止它持有的运行；
  下一个进程查到 `Finished { exit: Signal(15) }`，取消原因为 `Shutdown`。运行不能跨调用持有。
- `nohup sh job.sh &` 执行两次同一意图：派发两次，没有请求身份、去重、输出保留或停止确认。

宿主让运行的持有与客户端进程无关：客户端退出、超时或被中断不影响运行，之后的调用用同一身份
找回原运行、输出与停止事实。是否值得交给 Core，仍按契约中的[何时使用](MANAGED-RUN.md#何时使用)判断。

## 启动宿主

```sh
rho-core serve --project ~/work/proj --state-dir ~/work/proj-state \
  --executor sh=/bin/sh \
  --executor python=/usr/bin/python3 --executor-arg python=-u
```

宿主在前台运行，就绪后在标准输出打印一行 JSON：

```json
{"ready":{"socket":"/…/proj-state/host.sock","project_root":"/…/proj","state_dir":"/…/proj-state","pid":4242,"unreadable_records":0}}
```

| 选项 | 含义 |
| --- | --- |
| `--project DIR` | 项目根；运行的工作目录必须解析在其中 |
| `--state-dir DIR` | 持久记录目录；同一时刻只有一个 Core 使用它，套接字固定为其中的 `host.sock` |
| `--executor NAME=PROGRAM` | 可选执行方，程序为绝对路径，不查找 `PATH`；可重复 |
| `--executor-arg NAME=ARG` | 追加到该执行方脚本之前的参数；可重复，按出现顺序 |
| `--limit NAME=VALUE` | Core 限额：`max_running`、`max_records`、`max_code_bytes`、`max_args`、`max_arg_bytes`、`max_stream_bytes`、`max_output_chunks`、`max_read_bytes`、`stop_grace_ms`、`output_close_grace_ms` |
| `--max-connections N` | 同时处理的连接数，默认 16 |

选项也可写成 `--flag=value`。SIGTERM、SIGINT 与 SIGHUP 使宿主按下文“正常停止”退出；
启动时已被忽略的信号（如 `nohup` 下的 SIGHUP）保持忽略。宿主不改信号屏蔽字。

## Agent 调用

每个客户端命令需要套接字（`--socket PATH` 或 `RHO_CORE_SOCKET`）；除 `capability` 外还需要
调用方（`--caller NAME` 或 `RHO_CORE_CALLER`）。Agent 的各次 shell 调用之间环境变量可能不保留，
可以每次写出，或由 Agent 环境统一设置。

```sh
export RHO_CORE_SOCKET=~/work/proj-state/host.sock RHO_CORE_CALLER=agent-a
rho-core submit --id fit-001 --executor sh --workdir analysis --code-file fit.sh -- 100
rho-core wait --id fit-001 --timeout 60
rho-core read --id fit-001                      # 已保留的 stdout，从 offset 0 开始
rho-core read --id fit-001 --stream stderr --offset 4096
rho-core lookup --id fit-001 --record           # 附带带来源的已保存事实
rho-core list
rho-core cancel --id fit-001
rho-core forget --id fit-001
rho-core capability --contract                  # 打印契约全文
```

| 命令 | 作用 |
| --- | --- |
| `submit --id ID --executor NAME [--workdir DIR] (--code TEXT \| --code-file PATH\|-) [-- ARG...]` | 受理并启动，或返回该身份已有的受理；`--workdir` 默认项目根 `.`；`-` 从标准输入读脚本 |
| `wait --id ID [--timeout SECONDS]` | 最多等待到终态（默认 30 秒，至多 600 秒），返回当时的事实；超时只结束这次等待 |
| `read --id ID [--stream stdout\|stderr] [--offset N] [--max-bytes N]` | 读取一段已保留输出，单次至多 `max_read_bytes` |
| `lookup --id ID [--record]` | 立即返回已知事实；`--record` 附带完整操作记录（输出块列表改为块数） |
| `list` | 该调用方保留的全部操作的摘要 |
| `cancel --id ID` | 记录停止请求并向进程组发 SIGTERM，`stop_grace` 后 SIGKILL；确认看终态 |
| `forget --id ID` | 删除已确认结束、释放且事实已保存的记录；之后该身份可再次运行 |
| `capability [--contract]` | 项目根、执行方、Core 限额、宿主限额与读不出的记录 |

身份与调用方为 1–128 个 `[A-Za-z0-9._:@-]` 字符。身份命名该调用方的一个请求：同一内容再次提交
返回原运行且不再派发，内容不同为冲突；需要再次运行时使用新身份。

## 回复与退出码

回复是一行 JSON：成功为 `{"ok":true,"value":…}`，宿主报告的错误为
`{"ok":false,"error":{"kind":…,"message":…}}`，`message` 说明是否已有效果。运行视图包含
`request_id`、`run_id`、`terminal`、`held`、`status`、`cancel`、`stdout`/`stderr` 保留信息、
`unsaved`、`retention`、`accepted_at` 与请求摘要；`status` 等枚举保持 Core 的类型名，
如 `{"Finished":{"exit":{"Code":0},…}}`、`{"Detached":{…}}`、`{"Gone":{…}}`。时间为 UTC
RFC 3339、精确到毫秒。`read` 返回 `text`、`offset`、`next_offset`、`more_retained`、`lossy` 与
流信息：末尾被截断的 UTF-8 字符留到下一次读取，其他无效字节以替换字符表示并标记 `lossy`。
`capability --contract` 直接打印契约文本。

| 退出码 | 含义 | 调用方下一步 |
| --- | --- | --- |
| 0 | 宿主执行了请求（`wait` 返回时运行可能仍未结束） | 读 `value` |
| 1 | 宿主回答了错误，或请求超过宿主上限而未发送 | 按 `error.kind` 与 `message` 处理 |
| 2 | 用法错误，未发送 | 修正命令 |
| 3 | 未送达：没有宿主、宿主忙、拒绝或协议不符，未产生任何效果 | 稍后重试或启动宿主 |
| 4 | 已发送但没有回复，可能已执行 | 用同一身份重发或 `lookup`；不要换新身份重试 |

宿主错误的 `kind` 有 `invalid_request`、`unknown_executor`、`workdir`、`conflict`（附原运行
`existing`）、`capacity`、`shutting_down`、`record_unreadable`、`storage`、`invalid_identity`、
`not_found`、`offset_beyond_retained`、`protected`（附运行 `run`）、`io`、`bad_request`、
`request_too_large`；客户端自身的有 `host_unavailable`、`busy`、`refused`、`incompatible_host`、
`reply_unknown`。

## 保证与边界

- **持有**：运行由宿主中的 Core 持有。客户端进程退出、等待超时或被中断都不影响它。
- **重复送达**：去重由 Core 的请求身份决定，跨客户端进程、并发连接与宿主重启都成立。
  回复丢失（退出码 4）后用同一身份重发是去重，不是重放；重复 `cancel` 无副作用；
  重复 `forget` 可能得到 `not_found`。
- **未送达**：客户端只有在收到宿主的就绪问候后才发送请求，宿主只处理以换行结束的完整请求行，
  因此退出码 3 时宿主没有执行任何动作。
- **正常停止**（SIGTERM/SIGINT/SIGHUP）：不再接受新连接（已排队的连接在问候前关闭），
  删除自己的套接字，执行 `Core::shutdown()` 停止持有中的运行，最多再等 2 秒让进行中的请求返回，
  然后打印 `{"shutdown":{…}}` 报告（已确认停止、未确认、仍未结束的请求数）并退出。
  仍在进行的请求连接随进程关闭，其客户端得到退出码 4。
- **宿主崩溃或 SIGKILL**：运行继续但无人持有，套接字文件残留，客户端得到退出码 3。
  在同一 `state_dir` 上重启的宿主先取得存储锁，再替换残留的套接字（只替换套接字文件，
  不删除其他文件）；记录按契约报告为 `Detached` 或可确认的 `Gone`，不会再次派发。
- **独占**：第二个宿主在同一 `state_dir` 上于存储锁处失败（约 0.5 秒），不触碰正在使用的套接字。
- **信号**：Rust 标准库启动的子进程继承父进程的信号屏蔽字。宿主因此不屏蔽信号，而是用处理函数把
  信号写入关闭即执行（close-on-exec）的管道；exec 会恢复默认处理，运行收到 SIGTERM 即按默认动作结束。
  宿主启动时已被忽略的信号对运行同样保持忽略。

## 宿主拥有的资源与限额

| 资源 | 默认 | 配置 | 超限行为 |
| --- | --- | --- | --- |
| 同时连接 | 16 | `--max-connections` | 新连接收到 `busy` 问候后关闭，请求未读取，退出码 3 |
| 单个请求行 | `6 × (max_code_bytes + max_arg_bytes) + 3 × max_args + 64 KiB`，默认约 6.4 MiB | 随 Core 限额 | 客户端发送前拒绝（`request_too_large`，退出码 1）；不遵守上限的客户端收到同样的错误 |
| 每次套接字读写（问候、请求、回复） | 10 秒 | 固定 | 停滞的连接被丢弃；客户端在发送前为退出码 3，发送后为 4。按每次读写计时，持续缓慢发送的同用户客户端可占用连接更久 |
| 单次等待 | 600 秒 | 固定 | 客户端拒绝更长的 `--timeout`；宿主截断到 600 秒 |
| 客户端等待回复的每次读取 | 60 秒；`wait` 为超时加 10 秒 | 固定 | 退出码 4 |
| 客户端接收的回复 | 64 MiB | 固定 | 退出码 4 |
| 客户端读取的脚本 | 64 MiB | 固定 | 用法错误；真正的上限仍是 `max_code_bytes` |
| 停止时等待进行中的请求 | 2 秒 | 固定 | 关闭剩余连接，在报告中计数 |

每个连接占用一个线程，内存至多为一个请求行加一个回复；默认配置下请求缓冲的最坏总量约为
16 × 6.4 MiB。运行、记录、输出与读取的限额由 Core 负责，见契约中的[资源与超限](MANAGED-RUN.md#资源与超限)。
宿主不记录请求内容或输出。

## 安全

- 只监听 `state_dir` 中的 Unix 套接字，权限 0600；Core 创建 `state_dir` 时权限为 0700。
  没有网络监听、远程传输或令牌。
- 宿主用 `getpeereid`（macOS）或 `SO_PEERCRED`（Linux）检查对端 uid，不同于宿主有效 uid 的连接收到
  `refused` 问候。该检查未做行为测试（需要另一个用户）。
- 调用方名称是同一用户内的命名空间，不是认证：同一用户的任何进程都可以使用任何调用方。
- 运行以宿主用户的权限和环境执行；工作目录检查不是沙箱。见 [SECURITY](../SECURITY.md)。

## 线协议（内部）

每个连接一个请求：宿主先发一行问候（`ready` 带 `max_request_bytes`，或 `busy`、`refused`），
客户端再发一行请求，宿主回一行回复，均为 JSON。协议号为 1，只在同一构建的客户端与宿主之间使用，
不是公开 API；调用方使用 CLI。

## 验证范围

[`tests/host.rs`](../tests/host.rs) 以真实宿主进程与独立客户端进程覆盖：跨调用提交、找回、等待、
读取输出与参数、调用方隔离、删除；无宿主或残留套接字时不发送、不运行；8 个客户端进程并发同身份
提交只受理与派发一次，内容不同为冲突；经宿主取消以 SIGTERM 结束且未发 SIGKILL；宿主 SIGKILL 后
重启找回 `Detached`、重复提交为 `Existing`、任务结束后为 `Gone` 并可删除；宿主 SIGTERM 后运行停止、
套接字删除；第二个宿主被拒且不影响第一个；超限请求在 Core、客户端与宿主三处被拒；连接占满时回答
`busy`，宿主在回复前被杀时客户端报告 `reply_unknown`。只在 macOS 上运行；Linux 的对端 uid 读取
只在源码中存在，未编译或运行。Agent 经此 CLI 的真实使用评估尚未运行。
