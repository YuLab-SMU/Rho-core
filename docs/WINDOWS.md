# Windows 支持

`rho-core` 在 `x86_64-pc-windows-msvc` 上与 Unix 提供同一批行为回归
（lib、storage、activity、managed_run、restart、host、storage 套件全部通过）。
本页记录 Unix 原语到 Windows 的映射，以及由此产生的语义差异。

## 进程持有：Job Object 替代进程组

- 一个 run 由**命名 Job Object**（`Local\<boot-id>.<run-id>`）持有，包含执行器及其
  全部后代，与 Unix 的进程组对应。作业设置 `SILENT_BREAKAWAY_OK` 之外的常规限制，
  即默认不允许子进程脱离。
- 作业句柄由 holder 线程持有其一生。**命名的作业对象随最后一个打开的句柄消失**——
  即使其中仍有进程在运行——因此 holder 必须持住句柄；崩溃宿主之后，重启的 Core
  **无法**按名字重开该作业。
- 停止运行 = `TerminateJobObject`（一次终止，等效 SIGTERM+SIGKILL 合一）。run 的
  终止以 `Exit::Code(1)` 报告，与 Unix 的 `Exit::Signal(15)` 对应；两级停止
  （TERM 宽限后 KILL）在 Windows 上合为一次终止，宽限期仅在实验注入
  （`RHO_CORE_FAIL_AT=signal-term`）使第一次终止失败时才体现第二次。

## 缺失观察：pid 探测替代锁探测

- Unix 以 run 锁的 flock 证明 run 存活（fd 随继承存活）。Windows 的字节范围锁
  随句柄关闭即失效，且 MSYS 子进程不保留它，因此存在性由**记录的主 pid** 探测：
  `OpenProcess` 失败（文件不存在/参数无效），或能打开且退出码已设置（非
  `STILL_ACTIVE`），视为缺失。
- 一个已终止的进程在仍有句柄引用时保持可打开但镜像查询失败，因此退出码是缺失的
  证明，可打开性不是。
- pids 在 Windows 上会被快速复用。dispatch 记录了 spawn 观察时刻；当探测到
  `STILL_ACTIVE` 但进程创建时间晚于该记录时按复用处理（当前探测保留保守
  语义，复用检查的接入点在 `os::pid_absent_since`）。
- Windows 没有 pid 命名空间；`ProcessContext.pid_namespace` 恒为 `None`。

## 启动标识

`boot_id` 在 Windows 上是 ntdll `NtQuerySystemInformation(SystemTimeOfDayInformation)`
返回的系统启动时间的十六进制（同一 boot 内字节级稳定），跨重启改变即证明所有早先
的 run 已丢失，与 Linux 的 `boot_id` 语义一致。

## 宿主与 CLI：命名管道替代 Unix socket

- `rho-core serve` 监听命名管道
  `\\.\pipe\rho-core.<state-dir 全路径哈希>.host.sock`；路径哈希避免同机多项目的
  临时目录同名冲突。
- 客户端协议不变（greeting → 请求行 → 回复行）。
- 对端身份检查：宿主用 `GetNamedPipeClientProcessId` 打开客户端进程，比较其
  token user SID 与自身，等价于 Unix 的 peer-uid 检查。
- 宿主的优雅停止：SIGTERM/SIGINT/SIGHUP 在 Windows 上对应控制台 ctrl 事件
  （Ctrl-C / Ctrl-Break / 关闭）。控制台事件无法到达其他控制台的进程；测试夹具
  另设 `RHO_CORE_STOP_FILE`——该文件出现即视为 SIGTERM（仅测试使用）。
- 命名管道是内核对象，宿主死亡后不留下文件系统残骸；Unix socket 文件的清理
  步骤在 Windows 上不存在。

## 存储差异

- Unix 的 `0600`/`0700` 权限与 `O_NOFOLLOW` 在 Windows 上没有直接对应：
  目录/文件使用默认 ACL，symlink 替换攻击仅由 advisory lock 与 SQLite 自身的
  打开约束兜底（state-dir 锁文件以共享模式打开，排他性由锁内容保证）。
- `sync_dir` 在 Windows 上打开目录同步不被允许，改为同步树内一个已有文件；
  已提交事务的持久性由 SQLite 的 `synchronous=FULL` 保证。
- 路径统一去除了 `\\?\` 扩展前缀：SQLite 与 Rust 都接受扩展路径，但 MSYS
  程序（Git Bash 的 `sh`、`awk` 等）不接受，而 run 的执行器经常是它们。

## 已知差异（相对 Unix 验证过的范围）

- 进程组逃逸检测（`output_held_open_outside_the_group_is_reported_not_awaited`）
  依赖 `setpgrp`+`perl`，仅在 Unix 上运行；Windows 作业不允许静默脱离，
  等价的逃逸只能通过显式 breakaway 发生，spawn 会失败并按 NotStarted 报告。
- `closing_inherited_stdin_does_not_confirm_exit_after_host_crash` 仅在 Unix
  运行：Windows 上 run 锁不随宿主死亡存续，"锁释放不证明退出"的前提不成立。
- 成员进程比主进程活得久的场景（`a_group_member...`）在 Windows 上由 sh 包装器
  等待全部后台子进程，主 pid 的结束已隐含成员结束，中间的"成员仍占组"事实
  不可观测；测试按平台分别断言。
- 命令行长度上限 32 KiB（Unix 无此限制）；涉及超长参数的存储回滚测试使用
  24 KiB 参数压满页数达到同一效果。
- MSYS sh 会 re-exec：Core 记录的 spawn pid 是最初的 sh.exe 包装进程，它等待
  整个脚本进程树结束；测试里的 pid 探测以该包装进程为准。
