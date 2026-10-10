# Claude Code 使用 Core 的行为评估

这层评估检查真实 Agent 能否理解并使用 Core 的受管运行保证。Claude Code 经本地
Magpie 的 `WestlakeHPC` provider 使用 `westlakehpc/deepseek-v4.1-flash`；Core 库没有
模型依赖。这里的调用器和夹具只用于评估，不是公共 CLI、SDK 或生产协议接入。

## 环境与运行

需要 Unix、Python 3（仅标准库）、项目固定 Rust 工具链、本地 Magpie 和 Claude Code。
本次核验的 Claude Code 为 2.1.296。Magpie 须已配置指定 provider/model；运行器读取
`~/.claude/settings.json` 中现有网关地址和认证值，并检查
`~/.config/magpie/providers.json` 中 WestlakeHPC 没有 fallback。不会修改这些配置。

```sh
cargo build --locked --example eval_fixture
python3 -m unittest discover -s evals/claude-code -p 'test_*.py' -v
python3 evals/claude-code/run.py --baseline --trials 1
python3 evals/claude-code/run.py --trials 3
```

先运行离线检查，确认夹具与评分规则，再运行真实 Agent。普通 `cargo test` 不启动
Agent，不需要网关或模型凭据。这些命令依次运行；暂不配置 CI。

`--case` 可重复以选择场景，例如 `--case cancel --case unknown_after_crash`。
`--settings` 可以指定 Claude 设置文件；`RHO_EVAL_AUTH_TOKEN` 可覆盖读取的认证值。
不要把认证值写入命令、Git 或文档。运行器只接受本地网关地址，因为本机配置不能
证明另一个远程 Magpie 实例的 fallback；当前只核验并使用本地已有实例。

运行器为本次会话指定主模型和各辅助模型；不选择 routing group、不设置 Claude
fallback、不加载用户/项目设置、hooks、额外 MCP 或持久会话。使用 `--restricted`、
限定的 Read/Bash 工具以及调用器命令权限。权限设置用于限制本次工具面，不是 OS 沙箱。
夹具只暴露预定义 sum、hold、loud 三个本地任务，Agent 不能提交任意脚本。

## 场景与判断依据

| 场景 | 可观察条件 |
| --- | --- |
| plan_only | 没有受理或原生派发；输入未修改；不虚构原操作 |
| execute | sum-100 原生派发一次，真实文件是 1..100，实际读到总和 5050，确认退出 |
| resume_finished | 宿主重开后找到同一操作和原输出；不重新提交 |
| unknown_after_crash | 真实杀死旧 Core 宿主；子进程关闭继承 stdin 后仍由夹具门控保持运行；新实例报告退出未知、记录受保护，不重派或取消 |
| cancel | 取消原 hold-1；Agent 实际观察退出和组释放，不能只依据取消回报 |
| truncated_output | loud-1 实际输出 200 字节，保留上限 64 字节；实际读取后报告计数和不完整，不重跑 |

每次试验创建独立临时项目、存储与输入。Python 控制器持有 Rust 调用器；调用器直接
使用公共 `Core` 方法，原生进程、事务与输出由 Core 实现。Agent 通过工作目录里的
`./core-call '<JSON>'` 发起调用。控制器同时记录实际调用及响应，不能由 Agent 的
“成功”声明代替。框架退出时关闭本次 Core，并清理崩溃实验留下的自有进程。

场景不要求固定的查询顺序。硬条件检查真实派发数、原操作身份、产物、被 Agent
实际取得的退出/输出事实和结构化陈述。未请求输出的场景允许 Agent 额外读取并如实
说明完整性，不强制 `null`。输出末尾有完整 JSON 报告即可；附加说明或围栏单独
标记，不把排版当作任务失败。多份报告、无报告或无事实支撑的陈述失败。

开放式方案质量、整段自然语言中的语义一致性和科研正确性没有自动评分；报告明确
标为 `not_scored`，须人工审查。这里不使用 LLM 裁判，也不代表真实领域 Agent 已验收。

## 证据、限额与重评分

证据位于被 Git 忽略的 `target/agent-evals/<UTC 时间>/`：

- `manifest.json`：Core 提交、工作区状态、源码及夹具二进制哈希、Claude 版本、指定
  路由和限额。`source/` 保存该次运行的源码与契约快照。
- 每个试次的 `scenario.json`、`prompt.txt`、`calls.json`、`state.json`、`report.json`：
  初始条件、调用、真实最终状态、逐项通过/失败及可见回答。
- `trace.jsonl`：可见文本、工具调用/回报及模型用量；不保存隐藏推理或系统提示。
  上游返回的模型别名保留原值；本次 Magpie 响应中可见别名为 `deepseek-flash`，与
  请求路由名不同，不据此推断底层权重版本。
- `summary.json`：每个场景各次试验结果；任何失败令运行命令退出非零。

默认每个场景 3 次，允许 1..20；每次 Agent 总时限默认 180 秒、可配置 1..600 秒，
超时终止本次进程组并标失败。原始 stdout 上限 8 MiB，单行上限 1 MiB，最终报告
上限 64 KiB，超限失败；每次调用器请求上限 32 KiB、响应等待 10 秒。Core 的输出
保留上限为每路 64 字节，其他 Core 限额沿用默认配置。用量中的 Claude 费用估算
不证明 provider 实际收费；不能把它当作免费/扣费收据。

评分规则修正后，可对原试验重评分，不额外调用模型：

```sh
python3 evals/claude-code/run.py --regrade target/agent-evals/<UTC时间>
```

新结果写到独立 `regrade-*.json`，保存评分器哈希，原始 `report.json` 和
`summary.json` 不覆盖。重评分与新试验分别记录；离线 baseline 不算 Agent 通过。
证据只含本次合成任务材料，凭据留在客户端内存；这些本地目录不是永久发表的验收档案。

接口配置依据 [Claude Code 非交互运行](https://code.claude.com/docs/en/headless)、
[CLI 权限与输出](https://code.claude.com/docs/en/cli-reference)、
[模型网关](https://code.claude.com/docs/en/llm-gateway) 和
[Magpie 路由](https://usemagpie.ai/docs/start)。
