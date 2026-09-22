# Project guide / 项目说明

[README](../README.md) · [Architecture / 架构](ARCHITECTURE.md) · [Build / 构建](BUILDING.md)

## 中文

### 观察而非归因

RunDelta 比较受支持的执行和文件访问结果集合，以事件种类、操作与路径作为跨运行聚合身份；PID、时间、调度顺序和重复次数不作为该身份。失败的执行尝试也是观察，文本折叠或排序不能删除 JSON 证据。

原始证据文件及起止行号随每个规范化事件保存。读取历史记录不会使用新 parser 重新解释它们。记录中的路径是观察到的拼写，不是物理文件身份或内容证明。打开某个文件不证明其内容影响了运行结果。

### 文件系统上下文

`FsContext` 只执行有成功结果与已解析标志支持的转换：`CLONE_FS` 共享，fork/私有 clone 复制，成功的 `unshare(CLONE_FS)` 分离，经过证实的成功 `chdir`/`fchdir` 更新对应上下文。失败调用不执行转换。

绝对路径只能来自绝对系统调用参数或完整的该次 strace 路径/FD 注释。最近观察到的 CWD 只是上下文，不通过拼接、运行后查询或 `realpath` 建立物理路径身份。缺少足够证据的相对参数保留相对形式，并标记不确定。未知标志、重叠转换、namespace/root 变化、任务 ID 复用或不支持的身份变化都会降低完整性；不会猜测所有线程共享同一上下文。

支持的 trace 选择覆盖执行、文件访问、目录切换、创建进程/线程、部分 namespace、root 与 mount 转换、退出及 unfinished/resumed 形式；这不是所有 Linux 调用或 namespace/mount 行为的支持声明。

### 监督与资源边界

采集器和启动前版本探测都使用 pidfd、signalfd 与父进程死亡信号机制。进入目标执行边界时恢复调用方 HUP/INT/TERM 处理及完整信号掩码。监督前要求默认 `SIGCHLD` 且没有 `SA_NOCLDWAIT`。一旦 `pidfd_open` 返回 ESRCH 或等待返回 ECHILD，清理不会再向可能复用的数字 PID 发信号。

默认没有采集时长或 trace 大小限制。`--max-duration-ms` 和 `--max-trace-bytes` 是显式策略；大小按已打开的 trace inode 轮询，只在观察值严格大于限额时触发，可能超出。`--terminate-grace-ms` 默认 2000。达到限额先 TERM、宽限后必要时 KILL；因限额终止的记录保留失败与不完整事实，不能作为可信无差异结论。

版本探测在 `Command::spawn` 返回后有固定超时与有界输出；这不覆盖同步 exec-error 握手，也不能保证内核立即完成终止/回收。取消不能撤销目标已写入的数据或已发生的外部操作。特权或守护化 tracer wrapper 不属于直接子进程监督保证。

Rust 运行时或启动路径可能在 RunDelta 观察前改变忽略的 `SIGPIPE` 或已关闭的标准描述符。因此，对这些进程启动条件，不保证直接运行与经 RunDelta 运行严格等价。

### 存储与兼容

存储以打开的目录和文件描述符固定操作对象；标签只是名称，不作为路径。索引逐条扫描，不为每条记录一直保留描述符。实际 I/O 错误会在预留标签及启动目标前阻止录制，而不是把未知标签视为空闲；损坏记录应与健康记录隔离。

新记录格式为 metadata v3、events v2、finalization v1；diff JSON 为 v4。历史 metadata v1/v2 和 events v1 可读取，但未经校验的历史记录保持 unverified。v3 缺失最终标记为 unfinalized；有效标记的结构化校验不代表 capture 完整。corrupt、unverified、incomplete 与 verified 是不同维度的事实，不能相互替换。

最终标记最后写入，覆盖 metadata/events 的长度、事件数量与 SHA-256；不覆盖 raw trace、不认证来源、不抵御有意整体重写。发布成功后目录同步仍可能失败；“已发布”不等于“断电后保证存在”。恢复时不会制造最终标记。事件逐行验证，晚发 UTF-8/I/O 错误不能以已读前缀绕过检查。

### 使用边界

这是探索性 alpha，不承诺所有 Linux 工作负载。动态构建可能产生大量正常重复噪声；没有固定排名保证。追踪会影响时序。参数、路径、环境和原始证据可能含敏感信息，十六进制转义不是加密。分享前检查记录与报告。

## English

### Observation, not attribution

RunDelta compares aggregated outcome sets for supported execution and file access, using event kind, operation, and path as cross-run identity. PID, timing, scheduling order, and duplicate counts are not that identity. Failed execution attempts are observations too; text folding or ranking must not remove JSON evidence.

Every normalized event retains its raw evidence file and inclusive start/end lines. Historical records are not reinterpreted by a newer parser on load. Paths are observed spellings, not proof of physical file identity or contents. Opening a file does not prove its contents affected execution.

### Filesystem context

`FsContext` applies only transitions justified by successful outcomes and parsed flags: `CLONE_FS` shares, fork/private clone copies, successful `unshare(CLONE_FS)` splits, and verified successful `chdir`/`fchdir` updates the corresponding context. Failed calls do not perform transitions.

Absolute paths require an absolute syscall operand or a complete per-call strace path/FD annotation. Last-observed CWD is context, not physical identity reconstructed through concatenation, later filesystem queries, or `realpath`. Relative operands without sufficient evidence remain relative and uncertain. Unknown flags, overlapping transitions, namespace/root changes, task-ID reuse, and unsupported identity changes downgrade completeness rather than assuming all threads share a context.

The trace selection covers execution, file access, directory changes, process/thread creation, selected namespace, root, and mount transitions, exits, and unfinished/resumed forms. This is not support for every Linux syscall or namespace/mount behavior.

### Supervision and resource boundaries

The collector and pre-recording version probe use pidfds, signalfd, and parent-death signaling. The target execution boundary restores the caller's HUP/INT/TERM dispositions and complete signal mask. Supervision requires default `SIGCHLD` without `SA_NOCLDWAIT`. After `pidfd_open` returns ESRCH or waiting returns ECHILD, cleanup never signals a potentially reused numeric PID.

There is no duration or trace-size limit by default. `--max-duration-ms` and `--max-trace-bytes` opt into policy limits. Size is sampled from the opened trace inode and triggers only when the observation exceeds the limit, so it can overshoot. `--terminate-grace-ms` defaults to 2000. A limit sends TERM, then KILL if needed after grace; the resulting record retains failure and incompleteness, not a trusted equality conclusion.

The version probe has a fixed timeout and bounded output after `Command::spawn` returns. This does not cover the synchronous exec-error handshake or guarantee immediate kernel termination/reaping. Cancellation cannot undo target writes or external side effects. Privileged or daemonizing tracer wrappers are outside the direct-child supervision guarantee.

The Rust runtime or launcher may change ignored `SIGPIPE` or closed standard descriptors before RunDelta can observe them. Strict equivalence between direct execution and recording is therefore not promised for these startup conditions.

### Storage and compatibility

Opened directory/file descriptors anchor storage operations. Labels are names, not paths. Index scans consume records one at a time instead of retaining a descriptor for every record. Actual I/O failures block recording before label reservation or target startup; unreadable labels are not considered available. Damaged records are isolated from healthy records.

New records use metadata v3, events v2, and finalization v1; diff JSON uses v4. Historical metadata v1/v2 and events v1 remain readable, but legacy evidence without checksums stays unverified. A v3 record without its final marker is unfinalized. Verified structured storage is not proof of complete capture. Corrupt storage, unverified storage, incomplete capture, and verified storage are distinct facts, not interchangeable statuses.

The final marker is written last and covers metadata/events byte lengths, event count, and SHA-256. It does not cover raw traces, authenticate origin, or resist deliberate coordinated rewrites. Publication can succeed before directory synchronization fails: published does not mean guaranteed to survive power loss. Recovery does not manufacture a marker. Incremental event verification cannot accept a prefix after a late UTF-8/I/O failure.

### Limits

This exploratory alpha does not promise every Linux workload. Dynamic builds can produce substantial normal-repeat noise; no fixed ranking guarantee is made. Tracing affects timing. Arguments, paths, environments, and raw evidence may contain sensitive information. Hex escaping is not encryption; inspect records and reports before sharing.
