# Architecture / 架构

[README](../README.md) · [Project / 项目说明](PROJECT.md)

## Module boundaries / 模块边界

| Module | Responsibility / 职责 |
| --- | --- |
| `main` | Process boundary: dispatch, write output, map uncaught errors to exit 2 / 进程边界：分派、输出、未处理错误映射为 2 |
| `cli` | Declarative command input only / 只声明命令行输入 |
| `model` | I/O-free evidence, event, metadata, run, integrity types / 无 I/O 的证据、事件、元数据、运行与完整性类型 |
| `capture` | Linux supervision, raw-trace production, limits, capture-fact reduction / Linux 监督、原始 trace、限额与采集事实归纳 |
| `parse` | Pure incremental normalization and filesystem-context reasoning / 纯增量规范化与文件系统上下文推理 |
| `store` | Anchored storage, locking, labels, schemas, finalization, verification / 描述符固定的存储、锁、标签、格式、最终化与校验 |
| `diff` | Deterministic comparison and reliability of loaded runs / 对已加载记录确定性比较与可靠性判定 |
| `report` | Pure text/JSON presentation of computed results / 对已计算结果进行纯文本与 JSON 展示 |

```text
main ──> cli
  ├────> capture ──> parse ──> model
  │          └────> store ──> model
  ├────> store
  ├────> diff ──> model
  └────> report ──> model / comparison results
```

## 中文

依赖从流程与适配层指向稳定事实模型，不反向依赖 CLI、文件系统或进程。`parse` 不访问文件系统来“修正”路径；`diff` 不加载文件或启动进程；`report` 不重新计算可靠性或退出码。Linux `unsafe` 集中在系统边界，逐处记录安全前提；编译门禁拒绝无文档 unsafe 块及 unsafe 函数中的隐式 unsafe 操作。

`capture` 将版本探测、子进程身份、信号监督、软限额与目标/采集器完成事实分开建模。pidfd 保留进程身份，signalfd 将控制信号纳入监督循环。失去子进程身份时不回退为数字 PID 发信号。取消只尽力终止受监督生命周期，不能提供事务式回滚。

监督结束时冻结采集事实。最终化期间迟到的 HUP/INT/TERM 保持 pending，恢复调用方处理方式与掩码后按原语义交付，不倒写成目标被取消。`RunWriter::finish` 无论成功失败都消耗写权限，并保留锁及目录句柄直到最终化和清理结束；初始 metadata 的发布失败也是终态。未发布临时文件由资源所有者清理；发布后同步失败仍单独保留为 published-but-not-durable，不撤销已发布文件或伪造最终标记。

`TraceParser` 逐行接收输入；完成时解析任务代际与 `FsContext` 关系。原始行号及警告顺序保持不变。流式读取减少完整 trace 字符串的同时驻留，但规范化事件与上下文仍会占用内存，因此没有固定总内存上界。

`store` 持有目录、trace inode 和锁的描述符，避免路径重命名/替换把进行中的操作重新指向别处。事件 JSONL 写入、校验与摘要读取逐条处理；`show`/`diff` 仍收集事件以供展示/比较。最终标记只证明结构化存储完整性，不参与提升采集状态。旧内核缺少 `openat2` 时，单组件 `openat` 回退不能提供同设备 bind mount 的全部隔离保证。

CLI 文本与 JSON 消费同一计算结果，保留四态 diff 合同和已有格式版本；目标信号事实不被 `128 + signal` 数字替代。生产流程无数据库、网络服务、插件框架或多 crate 拆分要求。

## English

Workflow and adapter layers depend on stable facts, never the reverse. Models do not depend on CLI, filesystem, or process details. `parse` cannot query the filesystem to “correct” paths; `diff` does not load files or spawn processes; `report` does not recompute reliability or exit codes. Linux unsafe operations live at system boundaries with local safety explanations. Lints reject undocumented unsafe blocks and implicit unsafe operations inside unsafe functions.

`capture` separates backend probing, child identity, signal supervision, soft limits, and target/collector completion facts. Pidfds preserve process identity; signalfd integrates control signals into supervision. Lost child identity never permits fallback signaling by numeric PID. Cancellation is best-effort lifecycle termination, not transactional rollback.

Capture facts freeze when supervision ends. HUP/INT/TERM arriving during finalization stay pending and follow the caller's restored dispositions and mask; they do not retroactively become target cancellation. `RunWriter::finish` consumes write authority on success or failure, retaining its lock and directory handles through finalization and cleanup. Failed initial metadata publication is terminal too. Resource owners clean up unpublished temporary files. Publication followed by failed synchronization remains a separate published-but-not-durable outcome; it neither undoes publication nor invents a final marker.

`TraceParser` receives lines incrementally and resolves task generations and `FsContext` relationships at completion. Raw evidence coordinates and warning order are preserved. Streaming avoids retaining the complete raw trace string at once, but normalized events and context still consume memory: there is no fixed total-memory bound.

`store` holds descriptors for directories, the trace inode, and its lock so a path rename/swap cannot redirect an in-progress operation. Event JSONL writing, verification, and summary loading are incremental; `show`/`diff` still collect events for presentation/comparison. Finalization proves only structured storage integrity, never an upgraded capture state. On older kernels without `openat2`, the single-component `openat` fallback cannot provide every same-device bind-mount isolation guarantee.

CLI text and JSON consume the same computed results, preserving the four-way diff contract and schema versions. An explicit target signal fact is not replaced by the numeric `128 + signal` presentation. The production workflow requires no database, network service, plugin framework, or multi-crate split.

## Format compatibility / 格式兼容

| Format / 格式 | Current / 当前 | Compatibility / 兼容 |
| --- | --- | --- |
| metadata | v3 | Read v1/v2 without rewriting historical evidence / 读取 v1/v2，不重写历史证据 |
| events | v2 | Read v1, preserving raw coordinates / 读取 v1，保留原始坐标 |
| finalization | v1 | Missing markers stay unverified/unfinalized / 缺失标记保持未经验证或未最终完成 |
| diff JSON | v4 | Shared text/JSON reliability and exit codes 0/1/2/3 / 文本与 JSON 共用可靠性与四态退出码 |

## Deferred / 延后

- RF-630: remove compatibility adapters only after at least one stable release cycle; they remain in this candidate. / 兼容 adapter 至少经历一个稳定发布周期后才能删除；本候选继续保留。
- metadata v4 / diff v5: future schema design, not emitted or silently migrated here. / 后续格式设计，本版不输出、不静默迁移。
- Native startup shim: exact pre-Rust-runtime signal/descriptor preservation remains unimplemented. / 尚未实现原生启动层，不承诺 Rust 运行时之前的信号及描述符严格等价。
- Power-loss injection and special daemonize boundaries remain unvalidated; tests do not establish crash durability or arbitrary detached-descendant control. / 未验证断电注入及特殊守护化边界，不承诺断电持久性或任意脱离后代控制。
