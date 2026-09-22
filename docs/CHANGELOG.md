# Changelog / 变更记录

## v0.2.0-alpha.1 — source export candidate / 源码导出候选

This entry describes a local source candidate, not a published package or tag.
本条目描述源码候选，不表示已经发布 crate 或 tag。

### English

- Separated declarative CLI input, process dispatch, evidence models, parsing, capture, storage, comparison, and presentation responsibilities.
- Added pidfd/signalfd supervision for the collector and bounded backend version probe. Unsupported SIGCHLD dispositions are rejected before supervised spawn; lost child identity prevents numeric-PID cleanup signaling.
- Preserved caller dispositions for late finalization signals without rewriting frozen capture facts; made record finalization consume write authority on success or failure, keeping resources alive through unpublished-temporary cleanup.
- Added incremental trace parsing, JSONL event writing/verification, and bounded-descriptor index scans. Late UTF-8/I/O failures and index scan I/O failures remain fail-closed.
- Added explicit optional duration/trace-size limits and termination grace. Defaults retain unlimited duration/size and a 2-second grace; cancellation remains best effort.
- Modeled per-filesystem-context sharing and task generations from observed evidence. Unsupported or uncertain transitions cannot manufacture absolute paths or complete capture.
- Preserved metadata v3, events v2, finalization v1, diff v4, historical compatibility, raw evidence coordinates, and the four-way diff exit contract.
- Added self-contained public tests, source/package allowlists, encoded-content scanning, and full reachable-history checks. Buildability and archive testability are checked separately from runtime platform support.
- Added a read-only Linux GitHub Actions workflow: formatting, linting, unit/synthetic/real CLI tests, finalization signals, export counterexamples, history scanning, packaging, and unpacked-package testing. It has no publication step.

### 中文

- 分离声明式 CLI、进程分派、证据模型、解析、采集、存储、比较与展示职责。
- 对采集器和有界后端版本探测使用 pidfd/signalfd 监督；不支持的 SIGCHLD 处理方式在监督启动前拒绝；失去子进程身份后不使用数字 PID 清理。
- 最终化期间迟到信号保持调用方原有处理方式，不改写冻结后的采集事实；记录最终化无论成功失败都消耗写权限，资源保留至未发布临时文件清理结束。
- 增量解析 trace、写入/验证 JSONL 事件，并限制索引扫描同时持有的描述符；晚发 UTF-8/I/O 错误与索引扫描 I/O 失败保持 fail-closed。
- 增加可选时长、trace 大小和终止宽限；默认仍不限制时长/大小，宽限为 2 秒；取消仍是尽力而为。
- 从已观察证据建立独立文件系统上下文共享及任务代际；不支持或不确定的转换不能制造绝对路径或完整采集结论。
- 保持 metadata v3、events v2、finalization v1、diff v4、历史兼容、原始证据坐标与四态 diff 退出合同。
- 提供可独立运行的公开测试、源码/包白名单、编码内容扫描和完整可达历史检查；构建及解包测试成功不等于所有运行平台均受支持。
- 添加只读 Linux GitHub Actions：格式、lint、单测、合成及真实 CLI、最终化信号、导出反例、历史扫描、打包和解包测试；不含发布步骤。

### Deferred / 延后

RF-630 compatibility-adapter removal (after at least one stable release cycle), metadata v4/diff v5, a native startup shim, power-loss injection, and special daemonize boundaries are deferred. This release does not imply those designs or tests are complete.

RF-630 兼容 adapter 删除（至少经历一个稳定发布周期后）、metadata v4/diff v5、native 启动 shim、断电注入及特殊 daemonize 边界均延后；本版不表示这些设计或验证已经完成。

### Unchanged limits / 保持的边界

Differences are investigation clues, not root-cause proof. Finalization does not prove capture success or power-loss durability. Tracing may affect timing; cancellation does not undo side effects. Other platforms and all Linux workloads are not promised.

差异是排查线索而非根因证明。最终化不证明采集成功或断电持久性。追踪可能影响时序，取消不能撤销副作用。不承诺其他平台或所有 Linux 工作负载。
