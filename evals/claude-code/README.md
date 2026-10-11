# Claude Code 使用 Core CLI 的行为评估

这层评估检查真实 Agent 能否理解并使用 Core 的受管运行保证。Claude Code 经本地
Magpie 的 `WestlakeHPC` provider 使用 `westlakehpc/deepseek-v4.1-flash`；Core 库没有
模型依赖。默认使用真实 `rho-core serve` 和公共 CLI；测试控制器、命令入口与三个
预定义脚本仅用于隔离评估，不是另一套生产协议或 Agent harness。

## 环境与运行

需要 Unix、Python 3（仅标准库）、项目固定 Rust 工具链、本地 Magpie 和 Claude Code。
本次核验的 Claude Code 为 2.1.296。Magpie 须已配置指定 provider/model；运行器读取
`~/.claude/settings.json` 中现有网关地址和认证值，并检查
`~/.config/magpie/providers.json` 中 WestlakeHPC 没有 fallback。不会修改这些配置。

```sh
cargo build --locked --example eval_fixture
cargo build --locked --bin rho-core
python3 -m unittest discover -s evals/claude-code -p 'test_*.py' -v
python3 evals/claude-code/run.py --baseline --trials 1
python3 evals/claude-code/run.py --trials 3
```

先运行离线检查，确认夹具与评分规则，再运行真实 Agent。普通 `cargo test` 不启动
Agent，不需要网关或模型凭据。这些命令依次运行；暂不配置 CI。

默认 `--transport cli` 覆盖八类场景。`--transport fixture` 保留原库调用器的六类
场景，方便对照历史；不支持两个 CLI 送达失败场景。`--case` 可重复以选择场景，
例如 `--case cancel --case delivery_reply_unknown`。
`--settings` 可以指定 Claude 设置文件；`RHO_EVAL_AUTH_TOKEN` 可覆盖读取的认证值。
不要把认证值写入命令、Git 或文档。运行器只接受本地网关地址，因为本机配置不能
证明另一个远程 Magpie 实例的 fallback；当前只核验并使用本地已有实例。

运行器为本次会话指定主模型和各辅助模型；不选择 routing group、不设置 Claude
fallback、不加载用户/项目设置、hooks、额外 MCP 或持久会话。使用 `--restricted`、
限定的 Read/Bash 工具以及 `Bash(./rho-core *)`（旧夹具为 `Bash(./core-call *)`）
命令权限。权限设置用于限制本次工具面，不是 OS 沙箱。入口只接受预定义
sum、hold、loud 三个本地脚本与限定的 CLI 参数，Agent 不能提交任意代码。
`capability --contract` 按真实 CLI 输出契约文本，其他调用保留 JSON 回复与退出码。

组合任务时逐个调用并读取各次回报；文档和输入通过 Read 取得，不需要目录扫描或
外部格式化程序。Python、jq、管道、重定向等没有新增授权；原第二轮的 Python 管道
拒绝仍是失败。命令组合没有事务保证，一次失败不能触发整段重跑。

## 场景与判断依据

| 场景 | 可观察条件 |
| --- | --- |
| plan_only | 没有受理或原生派发；输入未修改；不虚构原操作 |
| execute | sum-100 原生派发一次，真实文件是 1..100，实际读到总和 5050，确认退出 |
| resume_finished | 宿主重开后找到同一操作和原输出；不重新提交 |
| unknown_after_crash | 真实杀死旧 Core 宿主；子进程关闭继承 stdin 后仍由夹具门控保持运行；新实例报告退出未知、记录受保护，不重派或取消 |
| cancel | 取消原 hold-1；Agent 实际观察退出和组释放，不能只依据取消回报 |
| truncated_output | loud-1 实际输出 200 字节，保留上限 64 字节；实际读取后报告计数和不完整，不重跑 |
| delivery_not_sent | 首次真实 submit 在不存在的测试宿主上得到退出码 3/host_unavailable；无受理与派发，测试宿主恢复后以原 sum-100 完成一次运行 |
| delivery_reply_unknown | 代理转发完整真实 submit 并取得宿主受理回复后丢弃客户端回复，CLI 得到退出码 4/reply_unknown；Agent 先查询原 sum-100，再取得原结果，不换身份 |

每次试验创建独立临时项目、存储与输入。默认控制器持有真实常驻宿主，Agent 用
`./rho-core submit/lookup/list/wait/read/cancel/capability`。评估入口校验脚本选择，
控制器随后启动真正的 `rho-core` 客户端并保留其退出码和回复；没有模拟受理、
派发或结果。旧 `--transport fixture` 经 `./core-call '<JSON>'` 使用公共库调用器。
控制器同时记录实际调用及响应，不能由 Agent 的“成功”声明代替。框架退出时
关闭本次测试宿主，并清理崩溃实验留下的自有进程；不处理用户宿主。

场景不要求固定的查询顺序。硬条件检查真实派发数、原操作身份、产物、被 Agent
实际取得的退出/输出事实和结构化陈述。未请求输出的场景允许 Agent 额外读取并如实
说明完整性。合法固定调用方对当前未保留的其他身份执行 lookup，实际得到
退出码 1/not_found，只作为一次负向读取；这不证明历史上从未受理，更不允许
重跑。原任务的 not_found、写入错误、范围错误与其他失败仍拒绝；原执行必须
由成功的查询实际取得。
未读取输出时完整性字段只能为 `null`。新试验用 Claude `--json-schema` 取得唯一四字段的
`structured_output`，并独立检查事实；成功结束但没有该输出仍失败。可见文本里的
重复或冲突报告仍失败，不能只挑最后一份或用合法结构化输出遮盖冲突。结果事件
对同一助手文本的镜像不算第二份报告；`StructuredOutput` 工具输入也计入报告，
同一工具 ID 的相同事件镜像只计一次，不同工具 ID 的多次提交仍失败。
历史重评分仍按原单份文本报告契约判断，
附加说明或围栏只作排版标记，不改写原失败。

开放式方案质量、整段自然语言中的语义一致性和科研正确性没有在这些合成场景中
自动评分；报告明确标为 `not_scored`，不把未评分范围解释为通过。真实领域任务
须另选原生可验证结果与独立确定性判断依据；这里不使用 LLM 裁判，也不代表
真实领域 Agent 已验收。

## 证据、限额与重评分

证据位于被 Git 忽略的 `target/agent-evals/<UTC 时间>/`：

- `manifest.json`：Core 提交、工作区状态、源码及夹具二进制哈希、Claude 版本、指定
  路由、transport、最终报告 Schema 和限额；CLI 模式同时保存真实 CLI 二进制哈希。
  `source/` 保存该次运行的源码与契约快照，包含宿主源码和 CLI 评估入口。
- 每个试次的 `scenario.json`、`prompt.txt`、`calls.json`、`state.json`、`report.json`：
  初始条件、调用、真实最终状态、逐项通过/失败及可见回答。CLI 状态还保留实际
  客户端参数、退出码与原始回复，以及送达故障发生处的真实宿主证据。
- `trace.jsonl`：可见文本、工具调用/回报及模型用量；不保存隐藏推理或系统提示。
  上游返回的模型别名保留原值；本次 Magpie 响应中可见别名为 `deepseek-flash`，与
  请求路由名不同，不据此推断底层权重版本。
- `summary.json`：每个场景各次试验结果；任何失败令运行命令退出非零。

默认每个场景 3 次，允许 1..20；每次 Agent 总时限默认 180 秒、可配置 1..600 秒，
超时终止本次进程组并标失败。原始 stdout 上限 8 MiB，单行上限 1 MiB，最终报告
上限 64 KiB，超限失败；每次调用器请求上限 32 KiB，控制器读取请求限时 10 秒。
CLI 入口等待限时 20 秒，真实客户端进程限时 15 秒；旧库夹具响应等待限时 10 秒。Core 的输出
保留上限为每路 64 字节，其他 Core 限额沿用默认配置。用量中的 Claude 费用估算
不证明 provider 实际收费；不能把它当作免费/扣费收据。

控制器被中断时也先停止本次 Agent 进程组，再关闭自己的夹具；已取得的调用、
可读取状态及 `interruption.json`/失败报告留在试次目录。中断不算通过。
普通异常保留已写出的失败报告；重评分保留异常、中断及证据不完整的试次为失败，
不会通过省略这些试次来改变通过率分母。
夹具初始化失败也保留场景和轮次；状态读取失败时另存 `cli-evidence.json` 中已有的
真实命令及故障证据，失败说明保留经认证值遮盖后的异常细节。

评分规则修正后，可对原试验重评分，不额外调用模型：

```sh
python3 evals/claude-code/run.py --regrade target/agent-evals/<UTC时间>
```

新结果写到独立 `regrade-*.json`，保存评分器哈希，原始 `report.json` 和
`summary.json` 不覆盖。重评分与新试验分别记录；离线 baseline 不算 Agent 通过。
证据只含本次合成任务材料，凭据留在客户端内存；这些本地目录不是永久发表的验收档案。

接口配置依据 [Claude Code 非交互运行](https://code.claude.com/docs/en/headless)、
[CLI 权限与输出](https://code.claude.com/docs/en/cli-reference)、
[结构化输出](https://code.claude.com/docs/en/agent-sdk/structured-outputs)、
[模型网关](https://code.claude.com/docs/en/llm-gateway) 和
[Magpie 路由](https://usemagpie.ai/docs/start)。
