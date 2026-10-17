> 公开树说明（2026-10-16）：本树引用的 ACCEPTANCE-before-reliability、FIELD_CASES、HOLDOUT_RESULTS、RANKING_REPAIR 及对应结果 JSON 为脱敏派生副本；原始证据保留在私有归档，未重新实测。下文关于私有树、历史路径或发布阻塞的结论属于当时状态，不代表本公开副本仍含这些路径。公开派生合同见 [REGRESSION_SET_PUBLIC.json](docs/REGRESSION_SET_PUBLIC.json)；原 v1 原件校验仍属私有树。
> Public references point to redacted historical derivatives, not fresh measurements. Original v1 evidence verification remains private; the public archive has a separate integrity contract.

# RunDelta

Linux 命令运行差异诊断工具。实际使用 strace 采集，比较执行尝试（含失败）的程序路径、文件访问路径及结果、初始目录、命令参数、选定的启动环境变量和退出状态。变化只是排查线索，不是根因证明。

A Linux command-run investigation tool using strace. It compares attempted executable paths, including failed attempts, file-access paths and outcomes, the initial directory, argv, selected startup environment variables and termination facts. Differences are investigation clues, never proof of a root cause.

## 开发门禁与历史验收 / Development gate and historical acceptance

开发门禁由 `python3 -B tests/full_gate.py` 编排，区分通过、失败和环境能力不足而跳过的检查。交付时应明确报告适用的 Rust、合同 golden、Linux/Python 回归结果，不能依赖 GitHub CI。历史报告的测试数量与实现描述只代表各自日期的快照。

The development gate is orchestrated by `python3 -B tests/full_gate.py`, which distinguishes pass, failure and skips caused by unavailable environmental capabilities. Delivery must report applicable Rust, contract golden and Linux/Python checks explicitly. Historical test counts and implementation descriptions apply only to their dated snapshots; GitHub CI is not a substitute for local validation.

早期可靠性修复见 [历史验收报告（公开脱敏说明）](docs/ACCEPTANCE.md) 与 [修复前报告（脱敏副本）](docs/ACCEPTANCE-before-reliability.md)。当前合同见 [FsContext 模型](docs/FS_CONTEXT_MODEL.md)、[格式与设计决策（公开合同）](docs/REFACTOR_DECISIONS.md) 和 [双语使用说明](docs/CRATE_README.md)。

Earlier reliability work remains in the historical acceptance reports. Current contracts are documented in the [FsContext model](docs/FS_CONTEXT_MODEL.md), [public format and design decisions](docs/REFACTOR_DECISIONS.md) and [bilingual usage guide](docs/CRATE_README.md).

```bash
cargo build
export RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace"
python3 -B tests/full_gate.py --core-only
```

专项脚本需要 Linux x86_64、cc、Python 3；可靠性回归还需要 pidfd 与 PTY。脚本在独立 target 目录操作数据，并向自己创建的进程发送信号；检测失败会返回非零，没有移除旧审计项。

Specialized scripts require Linux x86_64, cc and Python 3; reliability regressions also require pidfd and a PTY. They operate in separate target directories and signal their own processes. A failed check returns a nonzero status; existing audit checks remain part of the gate.

## 构建和运行 / Build and run

需要 Rust（edition 2024）、Linux 5.3+（可用的 pidfd 系统调用），以及支持下列选项/系统调用的 strace。历史本地验收使用 Rust 1.98.1、x86_64 Linux、strace 6.16；具体交付仍应绑定当次工具指纹。工具不会在 strace 不可用时退回无追踪执行。

Building requires Rust with edition 2024, Linux 5.3+ with usable pidfd syscalls, and strace supporting the documented options and syscalls. The historical local toolchain used Rust 1.98.1, x86_64 Linux and strace 6.16. RunDelta never falls back to executing an untraced target.

```bash
cargo build
# strace 已安装时无需设置；本次开发的本地构建可用：
export RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace"
target/debug/rundelta record good -- /usr/bin/true
target/debug/rundelta record bad -- /usr/bin/false
target/debug/rundelta diff good bad
```

被测命令通过独立参数启动，通常继承 stdin/stdout/stderr；Rust 入口前已关闭的 fd 0/1/2 或已忽略的 SIGPIPE 不保证严格保持调用者状态，详见 [运行时边界](docs/CRATE_README.md#observation-and-compatibility-boundaries)。没有 shell 字符串拼接。需要 shell 语法时显式传入 `sh -c ...`。默认存储于 `$XDG_DATA_HOME/rundelta`，否则 `$HOME/.local/share/rundelta`；可用全局参数 `--storage <目录>` 替换。`RUNDELTA_STRACE` 必须指向受信任、无 setuid/setgid/file capabilities 的 strace 可执行文件，不接受附加选项。fork/daemonize 包装器和自行解除监督的后端不在生命周期保证范围内。

Target arguments are passed separately, normally inheriting stdin/stdout/stderr. Descriptors 0/1/2 already closed before Rust startup and an inherited ignored SIGPIPE do not have strict preservation guarantees; see the [runtime boundary](docs/CRATE_README.md#observation-and-compatibility-boundaries). Use an explicit `sh -c ...` for shell syntax. Storage defaults to `$XDG_DATA_HOME/rundelta`, otherwise `$HOME/.local/share/rundelta`; the global `--storage <directory>` option overrides it. `RUNDELTA_STRACE` names one trusted strace executable with no setuid/setgid bits or file capabilities and accepts no appended options. Forking, daemonizing and self-detaching backends lie outside the direct-child lifecycle guarantee.

### 可选 capture 软上限 / Optional capture soft limits

录制时长及 raw 大小默认不限制。可显式设置 `--max-duration-ms`、`--max-trace-bytes` 和 `--terminate-grace-ms`（默认 2000 ms）。时长从正式 collector spawn 尝试前开始，不包括版本探测。raw 大小从锚定 inode 约每 50 ms 采样，并在解析前再检；仅 `observed > limit` 超限，允许越界，不向目标传播 RLIMIT_FSIZE。超限先 TERM、等待 grace、再按需 KILL/reap；成功保存的限额失败记录为 capture_failed/incomplete，record 返回 125。grace 内收到 recorder 信号时中断状态优先且限额 warning 保留。正式启动的 exec-error 握手与信号/限额共同轮询；回收使用非阻塞 wait 和独立有限清理预算，但单个内核操作或调度仍没有无条件硬期限。

Duration and raw size are unlimited by default. Explicitly set `--max-duration-ms`, `--max-trace-bytes` and `--terminate-grace-ms` (default 2000 ms). Duration starts before the formal collector spawn attempt and excludes the version probe. Size is sampled from the anchored inode about every 50 ms and rechecked before parsing; only observed > limit triggers, so overshoot is possible and RLIMIT_FSIZE is not propagated to the target. A limit sends TERM, waits grace, then uses KILL/reap if needed. A successfully saved limited record is capture_failed/incomplete with record=125. Recorder signals observed during grace take interrupted precedence while retaining the limit warning. The formal exec-error handshake is polled with signals and limits; reaping uses nonblocking wait and a separate finite cleanup budget. Individual kernel operations and scheduling still have no unconditional hard deadline.


## 记录管理与环境变量 / Record management and environment

```bash
target/debug/rundelta record good-env --env PATH --env PROBE_CONFIG -- /usr/bin/true
target/debug/rundelta list
target/debug/rundelta list --json
target/debug/rundelta show good-env
target/debug/rundelta show good-env --json
target/debug/rundelta delete good-env
```

show、diff、delete 接受名称或完整内部 ID；名称只是标签，即使包含 `../` 也不用于路径拼接。重名或与已有 ID 冲突会拒绝。delete 永久移除所选记录及其 raw 日志，不影响其他记录。list/show/delete 成功返回 0，操作级错误返回 2；list 会标记单条损坏并继续，list 返回 0 不代表全部记录健康。CLI 不自动请求交互确认。

Show, diff and delete accept a label or full internal ID. Labels, even `../`, are never joined into paths. Duplicate labels and collisions with IDs are rejected. Delete permanently removes the selected record and raw evidence. Successful list/show/delete return 0; operation errors return 2. List marks damaged rows and continues, so list=0 does not certify every record. Commands do not request interactive confirmation.

配置的存储根目录叶项拒绝 symlink，尾随 slash 不能绕过，祖先路径解析仍是打开边界。delete 在移除前只读检查 128 层目录深度预算（所选记录根目录为 0 层），删除时再次检查；预检时已有的超深树先返回 2。晚到外部变更或 I/O 失败仍可能部分删除；移除后发现所选目录 inode 仍有链接会报错，不能恢复已移除的替代叶项。删除不是事务，详见 [存储边界](docs/CRATE_README.md#data-privacy-and-removal)。

The configured storage-root leaf rejects symlinks, including paths with trailing slashes; ancestor resolution remains an opening boundary. Delete preflights a directory-depth budget of 128, with the selected root at depth 0, and checks it again during removal. An already over-budget tree fails before removal with code 2. Late external changes or I/O failures can still leave partial deletion. A post-removal linked-inode check reports an error but cannot restore a removed replacement entry. Deletion is not transactional; see the [storage boundary](docs/CRATE_README.md#data-privacy-and-removal).

`--env NAME` 可重复，快照明确区分未选择、选中但未设置（JSON null）、空值（空字符串）和非空值。v1 记录的环境选择为未知。环境差异按变量名展示。list/show 会验证新记录的存储摘要；list --json 的健康项保留平铺 metadata 字段，并添加 directory、storage_integrity 和适用的 raw_integrity；损坏项只有安全 ID/名称提示、目录和错误事实，不伪造 metadata。健康项包含已选变量值；分享导出文件前请审查。

Repeatable `--env NAME` distinguishes unselected variables, selected-but-unset values (JSON null), empty strings and nonempty values. Environment selection in v1 records is unknown. Differences are shown by variable name. List/show verify structured storage. Healthy list JSON rows retain flattened metadata plus directory, storage_integrity and applicable raw_integrity; damaged rows expose only safe ID/label hints, directory and errors. Healthy rows include selected values: review them before sharing.

结构化读写预算为 metadata 8 MiB、marker/raw manifest 各 64 KiB、JSONL 每行含 LF 8 MiB、events 文件 256 MiB、最多 1,000,000 事件及每份 JSON 最多 65,536 项（键、字符串、容器、标量）。超限归 unavailable，受影响 show/diff 返回 2、list 保留健康项；这不保证任意 OOM 恢复。旧 tuple 的 list 只增量校验事件；trace-v1 marker-v2 list 临时保留一条 run 的完整事件以验证 generation/provenance，构造 summary 后释放，最终列表无事件。索引和 summary 仍随记录数增长。

Structured read/write budgets are metadata 8 MiB, marker/raw manifest 64 KiB each, each JSONL line including LF 8 MiB, events file 256 MiB, at most 1,000,000 events, and at most 65,536 JSON values including keys/strings/containers/scalars per document. Exceeding a budget is unavailable: affected show/diff return 2 while list keeps healthy rows. This does not guarantee arbitrary OOM recovery. Old-tuple list validation is incremental; trace-v1 marker-v2 listing temporarily retains one run's complete event vector to check generations/provenance, then releases it when constructing the summary. Returned rows contain no events; index and summaries still grow with record count.


## 三个真实案例 / Three real cases

```bash
cargo build
python3 -B examples/run.py
```

另需 `cc` 和 Python 3。脚本编译一个微型 C 被测程序（RunDelta 本身为 Rust），每次创建独立 `target/acceptance-*` 目录，自动执行并断言：

The script additionally requires cc and Python 3. It compiles a small C target, while RunDelta itself is Rust, creates a separate `target/acceptance-*` directory, and asserts these cases:

1. `exe-good → exe-bad`：同一命令 `probe ok`，PATH 指向两个不同目录中的可执行文件，报告成功执行路径的消失和新增，不声称两个进程已精准匹配。
2. `config-good → config-bad`：同一程序通过启动环境选择 `old.conf` 或 `new.conf`，报告实际成功打开的配置路径变化。此案例未选择记录环境变量，报告证据来自文件访问；另有独立环境变量验收。
3. `missing-good → missing-bad`：同一路径先存在后删除，报告 `success → ENOENT` 和退出码 `0 → 1`。

The three cases compare executable-path observations after changing PATH, successful configuration opens selected by startup environment, and one path changing from success to ENOENT with target exit 0 to 1. The configuration case does not record its selector environment variable, and executable observations do not establish exact cross-run process matching.

脚本还验证重复正常运行无差异、子进程、chdir 后相对路径、含空格和引号的路径、非零退出、目标信号终止、向记录器发送 SIGTERM、strace 缺失/不可用、重名、损坏事件和不支持版本。新增验收覆盖 list/show/delete、环境值/未设置/空值、v1 字段缺失兼容、锁互斥、路径形式标签、失败采集记录和 fchdir。最后打印记录目录。重新比较，例如：

Additional assertions cover repeat runs, children, relative paths after chdir, spaces/quotes, nonzero exits, target signals, recorder SIGTERM, unavailable strace, duplicate labels, malformed events, unsupported schemas, list/show/delete, environment states, legacy compatibility, locks, path-shaped labels, failed captures and fchdir. The script prints its record directory; compare saved runs with the commands below.

```bash
target/debug/rundelta --storage target/acceptance-XXXX/records diff missing-good missing-bad
target/debug/rundelta --storage target/acceptance-XXXX/records diff config-good config-bad --json
```

每对报告保存为案例目录下的 `.txt`。`records/runs/<id>/` 包含 `metadata.json`、`events.jsonl`、`raw/trace.log`；新记录最终写入 `finalized.json`。报告 evidence 相对于存储根目录，含原始行号；跨行 syscall 给出首尾行。平面证据样本每个变化每侧最多三条；显式报告 v6 的 JSON 另提供变化键的全部已保存事件引用，范围见下文。

Each case saves text reports and records containing metadata.json, events.jsonl and raw/trace.log; new records finally publish finalized.json. Evidence paths are relative to the storage root and preserve original start/end lines, including resumed calls. Flat samples contain at most three entries per change per side. Explicit report v6 additionally includes all saved event references for changed keys within its documented scope.

## 退出码与完整性 / Exit status and integrity

`diff` 文本和 JSON 都分别表达观察差异、采集完整性、存储完整性，且同一输入返回相同退出码：

Diff text and JSON express observed differences, capture completeness and storage integrity separately and use the same computed exit status:

| 返回码 | 含义 |
|---:|---|
| 0 | 两侧支持范围内采集完整、所选报告合同要求的校验通过，未观察到差异 |
| 1 | 两侧支持范围内采集完整、所选报告合同要求的校验通过，观察到差异 |
| 2 | 损坏、格式不支持、记录不存在或其他工具错误；不输出普通比较结论 |
| 3 | 任一侧采集不完整/未知，或所选合同要求的证据未经验证/未完成；可能仍有可观察差异 |

| Code | Meaning |
|---:|---|
| 0 | Complete supported captures; required verification passes; no observed difference |
| 1 | Complete supported captures; required verification passes; an observed difference |
| 2 | Tool/load/corruption error; no ordinary comparison conclusion |
| 3 | Incomplete/unknown capture or required unverified/unfinalized evidence; observations may remain browsable |

JSON 使用 `observed_differences`（true/false/null）、`reliability.left/right.capture`、`reliability.left/right.storage`、`exit_code` 和 `errors`。无法读取损坏记录时 differences=null。文本在报告顶部显示同样状态；返回 3 时显示 `RELIABILITY NOT ESTABLISHED`，不能仅靠底部 warning 判断。0 不证明所有运行条件一致，采集完整只针对支持集合。

JSON separates observed_differences (true/false/null), per-side capture/storage reliability, exit_code and errors. Corrupt or unreadable comparison inputs yield null observations. Text shows the same facts at the top; code 3 displays RELIABILITY NOT ESTABLISHED. Code 0 does not prove equal execution conditions, and completeness is limited to supported events.

v4 的 0/1 要求结构化存储验证通过；v5/v6 还要求两侧 raw 完整性和保存的目标 provenance 验证通过。0/1/2/3 来自同一计算结果，排序与文本折叠不改变状态或删除 JSON 观察。

V4 requires verified structured storage for trusted 0/1. V5/v6 also require verified raw and validated saved target provenance on both sides. Ordering and text folding preserve the computed 0/1/2/3 status and every JSON observation.

- `record` 正常保留目标退出码；目标信号采用 128+signal 数值返回，不向自身重发信号。目标正常返回 143/162 与 SIGTERM/SIGRTMIN 在 metadata 中分开表达。
- 采集期间观察到记录器 SIGINT/SIGTERM/SIGHUP 时，若该信号继承为屏蔽或忽略，则直接取消 collector，由 guard 通过 SIGKILL 终止并回收；其他情况才转发给 strace 并进行宽限监督；没有工具错误并完成保存和信号恢复时，返回 128+该信号。正式采集中的 collector 监督/清理失败保留为独立采集器事实，由既有归约决定中断码或 125。初始化、存储及向上传播的工作流错误返回 2，包括记录已提交但信号恢复失败，或摘要的非 BrokenPipe 输出失败。目标也可能自行返回这些数值，应结合 metadata 与诊断区分。
- metadata 的 `exit_code`/`signal` 描述目标；`collector_exit_code`/`collector_signal` 描述 strace；`target_exit_raw` 保留根进程原始结束信息。未知信号保留原文、数值为空，采集标记 partial；不会仅因未知信号标为 capture_failed，此时 record 使用已观察到的采集器退出语义返回并警告。
- 采集状态 `running/completed/interrupted/capture_failed` 与采集完整性 `pending/complete_for_supported_events/partial/incomplete` 分开。目标正常被信号终止仍可 completed；记录器被中断为 interrupted。已知目标/采集器状态按退出码或信号的类型比较，不混用相同的 128+signal 数值。

Record normally preserves the target exit code and returns 128+signal for a target signal without resending that signal to itself. For recorder HUP/INT/TERM observed during capture, an inherited blocked or ignored signal cancels the collector directly, with guard SIGKILL termination and reaping; otherwise it is forwarded to strace with grace supervision; without a tool error, successful finalization and signal restoration allow an interrupted return of 128+signal. Collector supervision/cleanup failures during formal capture remain separate collector facts; the existing reduction chooses an interrupted code or 125. Initialization, storage and propagated workflow errors return 2, including failure to restore signals after commit or a non-BrokenPipe summary-output failure. These numbers may coincide with a target exit code. Metadata keeps target code/signal, collector code/signal and target_exit_raw separate; unknown target signals retain raw text, leave the number absent and reduce completeness. Capture state and capture completeness are separate facts; a signalled target can still have completed capture.

### 存储格式与校验 / Storage formats and verification

默认录制为 metadata v3/events v2/marker v1，默认旧记录之间的 diff 为 v4。显式 `record --evidence-contract trace-v1` 使用 metadata v4/events v3/raw manifest v1/marker v2，保留本 trace 的任务世代、目标 provenance 与实际解析 raw 字节的收据。成功加载的一侧是 metadata v4 时自动选择报告 v5；`diff --report-version 5` 或 `6` 显式选择格式，两侧加载失败也保持所选版本。历史记录不重新解析、重写或升级为 verified。

Default recording uses metadata v3/events v2/marker v1 and old-to-old diff v4. Explicit `record --evidence-contract trace-v1` uses metadata v4/events v3/raw manifest v1/marker v2 and preserves trace-local task generations, target provenance and the receipt of bytes actually consumed by normalization. A successfully loaded metadata-v4 side selects report v5 automatically. Explicit `diff --report-version 5` or `6` keeps that format even when both loads fail. Historical records are never reparsed, rewritten or promoted to verified.


trace-v1 的完整采集声明还要求每个已引入的 task generation 有 exit 观察；缺失时保留 warning 并降级，合法不完整记录仍可读且 diff 返回 3。每条 events-v3 exit（含 child）必须符合固定完成文本格式，单独 `success` 不算退出，非法格式为加载错误（diff 2）；历史 metadata v1–v3/events v1/v2 不重新解释。

A finalized trace-v1 record can claim complete supported capture only with an exit observation for every introduced task generation. Missing completion retains warnings and reduced completeness; otherwise valid incomplete data remains readable with diff=3. Every events-v3 exit, including child exits, must obey the fixed completion framing. Plain success is not an exit observation; malformed framing is a load error (diff=2). Historical metadata v1–v3/events v1/v2 keep their original interpretation.

报告 v6 增加 `evidence_contract="outcome_event_references_v1"` 和 `evidence_mapping`，按 kind/operation/path/outcome 保留变化键的全部已保存 normalized event 引用，包括重复坐标；索引从 0 开始，raw 行从 1 开始且含尾行。它不覆盖未变化键或未支持的 raw syscall，计数/PID/顺序不成为跨运行身份。样本仍每侧最多三条，省略数只描述样本。v6 先校验 DTO，再扣留合法 unfinalized 侧的 file/exec 观察及事件引用；metadata 比较仍保留，withheld 的事件数与省略数为 null、状态为 3。损坏输入仍为 2，普通 unverified 观察仍可浏览。详见 [全量 outcome 引用](docs/CRATE_README.md#full-outcome-references--全量-outcome-引用)。

Explicit v6 adds evidence_contract="outcome_event_references_v1" and evidence_mapping. Changed file/exec keys are grouped by kind/operation/path/outcome with every saved event index and original evidence range, including duplicate coordinates. Event indices start at 0; raw lines start at 1 and include the end line. Unchanged keys and unsupported raw syscalls are outside this mapping. Counts, PID and order do not become cross-run identity. Flat samples remain capped at three per side; omitted counts describe the sample only. V6 validates DTOs before withholding valid unfinalized file/exec observations and references. Metadata comparisons remain visible; withheld counts are null with status 3. Malformed data remains 2, and ordinary unverified observations remain browsable. See [full outcome references](docs/CRATE_README.md#full-outcome-references--全量-outcome-引用).

默认写入顺序：事件临时文件写完并同步→rename→读回检查内容/解析/事件数→最终 metadata 安全替换并读回→最后发布 finalized.json。trace-v1 在发布标记前还复核实际解析的 raw 收据并发布 raw manifest；marker v2 绑定该 manifest。标记包含 run ID、事件数、events/metadata 的字节数与 SHA-256。实现不会在这些提交前校验完成前发布标记；但标记 rename 成功后，目录同步仍可能失败，此时标记可能已可见，工具返回错误而不撤销成品。后续读取独立核验实际文件。采集失败但成功保存的记录也可以有标记。

Default finalization writes/syncs events to a temporary file, renames it, rereads and validates bytes/schema/count, safely replaces and rereads final metadata, and publishes finalized.json last. Trace-v1 also rechecks the raw bytes consumed by parsing and publishes the raw manifest before marker v2 binds it. The marker binds run ID, event count, metadata/events byte counts and SHA-256. It is not published before those pre-commit checks complete. After marker rename succeeds, directory sync may still fail: the marker may already be visible while the tool reports an error and retains the published files. Later readers independently validate the actual files. A capture that failed but was saved correctly can also have a marker.

读取时重新校验标记、字节数、摘要、事件数和 metadata 状态，而非信任 completed。存储完整性独立输出：

Loading rechecks the marker, bytes, hashes, event count and metadata state rather than trusting completed. Structured storage integrity is presented independently:

- `verified`：metadata/events 与最终标记一致。
- `unverified`：历史记录没有校验信息，明确显示“存储完整性未经验证”；不直接判为损坏。
- `unfinalized`：新格式缺少最终标记，或仍在运行的旧记录；不读取未最终完成的事件作为完整数据。
- `corrupt`：内容/数量不符、文件缺失或损坏；diff 返回 2、differences=null，show 报错，list 标记该行并继续。
- `unavailable`：diff 无法读取某侧，例如找不到记录或不支持的版本。

Verified means structured bytes agree with the marker. Unverified means legacy evidence lacks verification, not automatic corruption. Unfinalized means a required marker is absent or a legacy record is still running; events are withheld from ordinary complete-data use. Corrupt structured data produces diff=2 with null observations and show errors while list retains other rows. Unavailable covers missing, unsupported or otherwise unreadable records.

默认 marker v1 只校验 metadata/events，不覆盖 raw；trace-v1 的 marker v2 另绑定 raw manifest，raw 完整性单独呈现。两种 marker 都**不认证数据来源，不防止能同时改写文件与标记的恶意篡改**。使用 fsync/rename 是写入策略，**不是自动保证断电持久性**；未做断电或文件系统故障注入验证。完成标记不证明采集成功。

Marker v1 covers metadata/events, not raw. Trace-v1 marker v2 also binds the raw manifest, with raw integrity presented separately. Neither marker authenticates origin or prevents coordinated malicious rewriting. Fsync/rename is a write strategy, not a demonstrated power-loss guarantee; power-loss and filesystem-fault testing has not established one. Finalization does not prove capture success.

### 生命周期和锁 / Lifecycle and locking

结构仍是 RunDelta→strace→目标/子进程。在执行 strace 前设置父线程死亡信号 SIGKILL，并检查父进程，关闭父进程先死的竞态；strace 使用 `--kill-on-exit`。SIGKILL 记录器后由内核终止 strace，继而终止仍受追踪的进程，不依赖捕获 SIGKILL。信号转发使用 pidfd，避免回收后 PID 复用误杀，不广播进程组。通常保留会话、前台组及标准流；PTY 输入和 Ctrl-C 有历史实测，仍受上述 Rust 启动前边界限制。

The process relationship is RunDelta → strace → target/children. Before executing strace, RunDelta sets PDEATHSIG=SIGKILL and checks the parent to close the parent-death race; strace uses --kill-on-exit. Recorder SIGKILL therefore asks the kernel to terminate strace and still-traced tasks. Signal forwarding uses pidfd rather than reused numeric PIDs or process-group broadcasts. Normal session, foreground group and standard-stream inheritance has PTY/Ctrl-C coverage, subject to the pre-Rust runtime boundary above.

`record` 在获得采集资源前，以相同 PID 重执行当前 `/proc/self/exe` inode，建立单线程采集所有者。旧可执行镜像中的线程、signal handler 地址和 reaper 不会进入新镜像；这改变了旧版本“发现已有线程或忽略 SIGCHLD 就拒绝”的入口行为。仅为所有者启动移除 `LD_*` 和 `GLIBC_TUNABLES`，通过密封匿名 FD 传入原生字节请求，随后由独占 guard 恢复完整原始 environment 列表（含重复名称、非法条目、原顺序），后端及 target 保留原环境。目标命令参数、cwd、PID、标准 IO 和会话继承；标准 IO 关闭槽与 SIGPIPE 仍按既有 Rust startup 边界处理；请求按现有内核 argv/environment 预算推导边界，不添加证据或输出配额。私有入口不是公共 API 或来源认证。正常非录制命令不进入该所有者。

Before acquiring capture resources, `record` reexecutes the current `/proc/self/exe` inode with the same PID into a single-threaded owner. Threads, handler addresses and reapers in the old image do not survive. This replaces the earlier refusal of existing host threads or ignored SIGCHLD. Loader entries `LD_*` and `GLIBC_TUNABLES` are removed only for owner startup; a sealed anonymous FD carries native-byte input, and an exclusive guard restores the complete original environment list, including duplicate names, malformed entries and order, before backend/target execution. Target argv, cwd, PID, standard IO and session are inherited; the established Rust-startup closed-descriptor and SIGPIPE boundaries still apply. The transport bound is derived from the existing kernel argv/environment budget, not an evidence/output quota. The private entry is neither a public API nor source authentication. Other commands do not enter this owner.

`exec` 按平台语义把 caught HUP/INT/TERM/SIGCHLD callback 重置为默认；旧镜像 callback 不再被调用，不能恢复旧函数地址。默认/忽略 disposition 和完整 mask 的跨 exec 语义保留；所有者临时规范化 SIGCHLD 以回收自身 child，child 启动时恢复跨 exec 可继承的原状态。因此在事实冻结后到来的信号若旧状态为 caught，现在可按默认动作终止 CLI；记录的 target/collector 事实不随之改写。此 binary-only 入口不提供可返回的多线程 embedding API。所有者及其 environment/signal/reaper guard 不可跨线程移动或共享；当前所有者调用图不创建宿主线程、不修改 environment、不重载旧宿主 hook。原始 environment 快照期间非协作 native `setenv` 或外部注入仍不在保证范围内。

By platform exec semantics, caught HUP/INT/TERM/SIGCHLD callbacks become default actions: old image callbacks are not invoked and their function addresses cannot be restored. Default/ignored dispositions and the complete mask retain their cross-exec semantics. The owner temporarily normalizes SIGCHLD to reap its children and restores the inheritable state before child exec. A signal arriving after fact freezing may therefore terminate the CLI by its default action where an old caught handler previously ran; this does not rewrite target/collector facts. This binary-only entry is not a returning multithreaded embedding API. Environment/signal/reaper guards cannot move/share across threads; the current owner callgraph creates no host threads, mutates no environment and reloads no host hook. Concurrent noncooperating native `setenv` during the original snapshot and external injection remain outside the guarantee.

版本/selector 探测从私有 fork 前计时 5 秒，父进程先持有 child/pidfd，再共同轮询 exec-error 管道、控制信号与原有 64 KiB stdout/stderr（诊断最多 4 KiB）。管道仅非阻塞排空已有字节，不为后代持有写端等待 EOF。每次显式回收使用 WNOHANG 与独立 5 秒用户态预算；普通回收失败后再做清理，二者最多两个预算，不能概称所有阶段共 5 秒。未观察到回收时先撤销数字 PID 权限，返回工具错误 2；正式采集保留可定位 running/未定稿目录，不读取、冻结或发布其证据，版本探测则不建立记录。信号恢复仍在错误退出之外执行。Drop 只做非阻塞尽力清理，不证明成功；单个内核调用和调度仍可能停滞。

Version/selector probing starts its five-second clock before private fork. The parent owns the child/pidfd before polling the exec-error pipe, controls and existing 64 KiB stdout/stderr streams (4 KiB diagnostic view). Pipes drain available bytes nonblockingly; descendants retaining write ends do not require waiting for EOF. Each explicit reap uses WNOHANG and a separate five-second userspace budget; failed normal reap followed by cleanup can consume two budgets, so the whole operation is not a global five-second guarantee. Without an observed reap, numeric-PID authority is revoked and the tool returns 2. Formal capture retains a locatable running/unfinalized directory without reading, freezing or publishing its evidence; preflight admits no record. Signal restoration still encloses error exit. Drop makes nonblocking best-effort cleanup only, not proof of success; individual kernel calls and scheduling can still stall.


fork 子进程、仍受追踪的 setsid 子进程已实测；主动解除 ptrace、特殊权限/命名空间、双重 fork 守护化、不可中断内核等待等没有完整保证。终止是异步的，不能承诺瞬时无进程；僵尸回收由父进程/系统 init 完成。SIGKILL 没有用户态收尾，落盘状态取决于中断点：提交未完成的合法新记录为 unfinalized，diff 返回 3；若标记已发布则按实际文件正常校验，损坏仍返回 2。初始占位可能仍为 running/pending，但不能由 SIGKILL 一概推定状态或标记缺失；不回滚已发布成品，也不补造标记，尚不自动恢复。

Fork and still-traced setsid children have historical test coverage. Active ptrace detachment, special privilege/namespace behavior, double-fork daemonization and uninterruptible kernel waits lack a complete guarantee. Termination is asynchronous; no instantaneous empty-process claim is made. Zombies are reaped by parents/system init. Recorder SIGKILL cannot run user-space cleanup, so persisted state depends on the interruption point. Valid new records whose commit is unfinished are unfinalized with diff=3; already published markers are checked normally, and corruption still returns 2. An initial placeholder may remain running/pending, but SIGKILL alone establishes neither that state nor marker absence. Published files are not rolled back and markers are never fabricated. Automatic recovery is not implemented.

存储使用 `.record.lock` 上的内核 flock，记录与删除互斥，进程退出自动释放；不要手动删除活跃锁文件。锁释放不代表异步进程清理已在同一时刻结束。没有活动 recorder 时可 delete 清理遗留记录。

Kernel flock on .record.lock serializes record/delete and releases on process exit. Do not remove an active lock file. Lock release does not certify that asynchronous process cleanup has already ended. With no active recorder, delete can remove a leftover record.

结构化提交前冻结 capture 事实；之后到来的 HUP/INT/TERM 保留为 pending，在成功或失败清理时恢复新镜像可继承的 disposition 与完整 mask。默认迟到信号可能终止 recorder，但不变成 target signal、不回写 metadata。已提交记录与后续 recorder 信号恢复失败可以同时存在，错误保留已提交 ID；工具错误不能伪装为未提交。

Capture facts freeze before structured commit. Later HUP/INT/TERM remain pending until cleanup restores the new image's inheritable dispositions and the complete mask on success or failure. A default late signal can terminate the recorder without becoming the target signal or rewriting metadata. A committed record can coexist with a subsequent recorder signal-restoration failure; the error preserves its ID and must not present that committed fact as an uncommitted record.

### 输出错误与内存边界 / Output errors and memory limits

main 负责输出和 flush。stdout 的 BrokenPipe 保留已计算状态（包括 diff 0/1/2/3），其他写入/flush 错误返回工具错误 2；record 提交后 stderr 摘要采用同样政策，错误链包含已提交 ID。diff、show 和 list JSON 写入 8 KiB 缓冲并显式补 LF/flush；失败可能留下 JSON 前缀，缓冲以 into_parts 拆解避免 Drop 重试。list JSON 保留完整已验证索引，依次构造和释放一个平铺兼容项，不再额外保留全列表展示副本或完整编码字符串。完整事件、Report、v6 引用及展示向量仍有线性成本；text 和 record 摘要仍构建完整字符串。不保证原子 stdout 或任意 panic/OOM 恢复。

Main owns output and flush. Stdout BrokenPipe preserves the computed status, including diff 0/1/2/3; other write/flush failures become tool error 2. The post-commit record stderr summary follows the same policy and its error chain retains the committed ID. Diff, show and list JSON write through an 8 KiB buffer with explicit final LF/flush; failure may leave a JSON prefix, and into_parts prevents Drop from retrying buffered bytes. List JSON retains the complete validated index, constructs and releases one flat compatibility row at a time, and keeps neither a second whole-list view nor a complete encoded string. Complete events, Report, v6 references and presentation vectors retain linear costs; text and record summaries still build complete strings. Atomic stdout and arbitrary panic/OOM recovery are not established.


## 解析覆盖与限制 / Parsing coverage and limits

实际选项经过本机 `strace --help` 和真实输出验证：`-f -q -I 2 --kill-on-exit -xx -yy -s 65535 -e trace=... -o <日志>`。本地工具源自 [strace 官方发布站](https://strace.io/)，未将其源码合入 RunDelta；下载与构建文件只在被忽略的 target 中。

The local historical strace help and real output supported `-f -q -I 2 --kill-on-exit -xx -yy -s 65535 -e trace=... -o <log>`. The separately built backend came from the [official strace release site](https://strace.io/); its source is not part of RunDelta and local download/build artifacts stay in ignored target directories.

明确采集/解析：

Supported capture/normalization forms include:

- `execve`、`execveat`：保存执行尝试及结果，成功和失败尝试均参与比较，涉及失败时单列 Execution attempt changes；可完整解析时保存 argv 数组，缩略参数显示警告并保留原始证据。
- `open`、`openat`、`openat2`、`access`、`faccessat`、`faccessat2`：操作、路径、成功或 errno。成功 open 并不证明读取过内容。
- `chdir`、`fchdir`：目录事件；`clone`、`clone3`、`fork`、`vfork`：父 PID 与返回的子 PID；strace 进程退出记录。
- `unshare`、`setns`、`chroot`、`pivot_root`、`mount`、`umount2`、`move_mount`、`mount_setattr`：检测成功的上下文或挂载拓扑改变并保守降级，不模拟完整命名空间。
- 按 PID 拼接 unfinished/resumed 调用，保存两端行号。未匹配、未知、截断、缩略数据都会产生警告。覆盖计数是日志行数与成功解析事件数，不代表全体系统调用覆盖率。

Execve/execveat retain successful and failed attempts, outcomes and fully decoded argv when available; abbreviated argv warns. Open/access families retain operation, path and success/errno: a successful open does not prove a read. Chdir/fchdir, clone/clone3/fork/vfork and exit rows retain context/lifetime observations. Successful unshare/setns/chroot/pivot_root/mount-family calls conservatively invalidate context without simulating namespaces. Unfinished/resumed rows are joined by observed task lifetime with both raw bounds. Unsupported, unmatched or truncated data warns. Parsed-event/log-line counters do not measure all-syscall coverage.

绝对路径保留原样，仅由绝对 syscall 参数或完整的当次 strace 路径/FD 注释建立，不消除 `..`，不做事后 realpath。初始 cwd、成功 chdir/fchdir 与 fork/clone 继承只提供最近观察到或词法描述的 `cwd_before` 上下文，不能拼成当前绝对访问路径。相对访问缺当次路径证据时保留相对操作数，设 `context_uncertain`、发出 warning 并降低采集完整性；成功 open 系列可使用当次返回 FD 的完整路径注释，这仍不证明物理身份或原子时刻。

Absolute spellings remain unchanged and require an absolute syscall operand or complete per-syscall strace path/FD annotation. No realpath lookup removes .. or reconstructs identity after a run. Initial CWD, observed directory changes and inheritance provide last-observed/lexical cwd_before only. Insufficient relative-path evidence retains the operand, context_uncertain and a warning with reduced completeness. A complete returned-FD annotation may establish an open-family path, but never physical identity or an atomic historical instant.

成功调用与完整 flags 证明 `CLONE_FS` 时共享局部 FsContext；私有 fork/clone 复制它，成功 `unshare(CLONE_FS)` 拆分它，已验证的成功 chdir/fchdir 只更新对应上下文，exec 保留上下文。失败调用不转换。未知 flags、重叠转换、任务身份和根与命名空间变化按受影响范围失败闭合；尚未建模 mount-namespace membership 的成功挂载拓扑调用和 pivot_root 使全部活跃上下文不确定。任务世代区分已观察的生命周期，不认证物理进程身份。目录重命名与完整命名空间语义仍不模拟；无证据的 fchdir、空路径 execveat 和未支持身份转换保留未知。权威合同见 [FsContext 模型](docs/FS_CONTEXT_MODEL.md)。shebang 内核加载的解释器未必另有 execve 事件；成功 exec syscall 的路径不代表全部内核解释器加载。

A successful call and fully decoded CLONE_FS flags prove local FsContext sharing. A proved private fork/clone copies context; successful unshare(CLONE_FS) splits it; verified successful chdir/fchdir updates that context only. Exec preserves context and failed calls do not transition. Unknown flags, overlapping changes and uncertain task/root and namespace identity fail closed. Without modeled mount-namespace membership, successful mount-topology calls and pivot_root make every active context uncertain. Task generations describe observed lifetimes, not physical-process authentication. Renames, complete namespace semantics, insufficient fchdir, empty-path execveat and unsupported identity transitions stay outside stronger claims. Shebang interpreter loading may have no separate execve event. The [FsContext model](docs/FS_CONTEXT_MODEL.md) is authoritative.


运行级集合比较忽略 PID、时间、调度顺序及重复次数；区分操作和结果，不匹配跨运行进程。不会忽略系统路径。文件内容、网络、stat/readlink 等其他 syscall、文件描述符读取行为、完整环境和进程级参数比较不在本阶段范围。没有历史记录不能恢复过去；相同路径不保证相同内容；追踪开销可能影响时序。

Cross-run comparison aggregates outcome sets by kind/operation/path, ignoring PID, timing, scheduling order and duplicate count. It does not match processes or ignore system paths. File contents, network, other syscalls such as stat/readlink, descriptor read behavior, complete environments and per-process argv matching remain outside this scope. Equal paths do not prove equal content. Unrecorded history cannot be recovered and tracing may alter timing.

路径和命令的非打印/非 ASCII 字节以 `\xNN` 可逆表示，反斜杠自身也转义；raw 使用 strace 十六进制字符串。记录目录权限 0700。参数及原始日志可能含密钥，文本和 JSON **不自动脱敏**，分享前自行审查。仅记录用户用 --env 显式选择的变量；选中值会原样存储并在 show/diff 的文本及 JSON 中显示，未选择变量不会进入环境快照。快照只描述启动时状态，不能证明子进程后续环境相同。

Nonprinting/non-ASCII path/command bytes are reversibly escaped as \xNN, including backslashes; raw strace strings use hexadecimal escapes. Run directories are mode 0700. Arguments, paths, raw logs and selected environment values may contain secrets and are stored/displayed without automatic redaction. Only explicitly selected startup variables enter the snapshot, which cannot prove later child environments match. Review reports and raw data before sharing.

## 测试与依赖 / Tests and dependencies

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
python3 -B tests/full_gate.py --core-only
# 环境满足 Linux/strace/pidfd/PTY/namespace 等能力时：
python3 -B tests/full_gate.py
```

默认测试使用固定日志 fixture，不依赖 strace；真实采集测试显式 ignored，须满足上面的 Linux 工具条件后启用。测试不伪造采集结果。

Default tests use frozen trace fixtures and need no strace. Real capture tests are explicitly ignored until the documented Linux capabilities are available. Tests do not fabricate captures.

统一门禁也执行全目标构建、严格 Rustdoc、独立进程所有权测试及单独的 user/network namespace 测试。`--release` 要求所有检查通过且零跳过；普通模式逐项报告能力不足，`--core-only` 显式跳过环境测试。

The unified gate also runs all-target builds, strict Rustdoc, process ownership tests, and a separate user/network namespace suite. `--release` requires every check to pass with zero skips; ordinary mode reports capability limits per check, and `--core-only` explicitly skips environmental tests.

依赖用途：clap 解析 CLI 并保留参数边界；serde/serde_json 保存版本化记录与 JSON 报告；anyhow 提供错误上下文；libc 提供 signalfd/pidfd/poll、flock 与父进程死亡通知；sha2 用于标准 SHA-256 存储摘要。无数据库或采集后端抽象层。

Production dependencies are clap for CLI argument boundaries, serde/serde_json for versioned records and JSON views, anyhow for error context, libc for signalfd/pidfd/poll/flock/PDEATHSIG, and sha2 for SHA-256. There is no database or tracing-backend framework.

更多特殊权限、ptrace 脱离、复杂命名空间、不可终止内核等待和断电边界仍有限制；没有自动崩溃恢复或跨运行进程精准匹配。当前实现及有意保留的边界见 [公开重构状态](docs/REFACTOR_STATUS.md) 和 [双语使用说明](docs/CRATE_README.md)。

Special privilege, ptrace detachment, complex namespaces, uninterruptible waits and power loss retain limits. Automatic crash recovery and exact cross-run process matching are not implemented. See the [public refactor status](docs/REFACTOR_STATUS.md) and [usage guide](docs/CRATE_README.md).

## 真实流程评估 / Historical workflow evaluation

原评估前五条直接线索命中 3/5；修复 resumed 对齐空格解析、失败 exec 漏报并调整排序后，重新采集的五例达到 5/5，直接线索均为第 2 条。该历史批次的 Cargo 按当时共享 FS 模型保存了不完整状态（diff 3），不能代表当前新鲜采集的预期。正常重复 Cargo 的历史报告保留 1598 条路径差异，文本折叠为 724 个条目/分组，没有删除观察事实。详见 [修复验收与复现命令（脱敏历史副本）](docs/RANKING_REPAIR.md)、[结果 JSON（脱敏副本）](docs/RANKING_REPAIR_RESULTS.json)、[原始评估（脱敏副本）](docs/FIELD_CASES.md) 和 [历史/新鲜审计分层](docs/FIELD_AUDIT_LAYERS.md)。

The historical original five cases improved from 3/5 to 5/5 top-five direct clues after resumed parsing, failed-exec reporting and ordering repairs; all direct clues ranked second in that dated batch. Its Cargo evidence was saved as incomplete under the then-current FS model (diff=3), not a prediction for current fresh captures. Normal-repeat Cargo retained 1598 path changes, folded into 724 text entries/groups without deleting observations. The linked historical reports/results and audit-layer document retain that scope.

排序优先展示单一 success→错误和失败执行尝试。文本前五条完整展示，之后仅折叠单侧失败文件访问与窄规则识别的临时路径候选，显示数量和示例排名；`diff --json` 包含全部原路径、结果、证据样本与 nullable `detail_group`，v6 另保留全量引用。临时规则是展示启发式，不忽略数据或改变退出码；具体规则及局限见修复报告。报告版本选择见上文，旧记录不自动重新解析。

Ranking prioritizes isolated success-to-error transitions and failed execution attempts. Text keeps the first five entries and folds only later one-sided failed access or narrow temporary-path candidates, retaining counts/examples/ranks. JSON preserves every change, original path, outcomes, samples and nullable detail_group; v6 also retains full references. Folding is a presentation heuristic and cannot alter reliability or status. Saved historical records are not reparsed.

## 固定回归与独立评估 / Frozen regressions and independent evaluation

原五例现为 [固定回归集 v1](docs/REGRESSION_SET.md)，不再用于调整排序。新增五例先登记预期再执行，前五命中 4/5；定位判断为 2 个可定位、2 个部分线索、1 个不能（分析者判断，未做独立用户测试）。见 [新五例报告](docs/HOLDOUT_EVALUATION.md)。排序与产品源码保持不变。

The original five cases are the [frozen regression set v1](docs/REGRESSION_SET.md), not ordering-tuning inputs. A separately preregistered historical holdout had 4/5 top-five hits: two locatable, two partial clues and one not locatable, as analyst judgments without an independent human-user study. See the [holdout report](docs/HOLDOUT_EVALUATION.md); that evaluation preserved product source and ordering.

冻结历史 Cargo 记录的采集不完整但仍能浏览已观察差异，**对这些保存记录，diff 返回 3 是正确行为**；当前 parser 不追溯重写它们。当前局部 FsContext 的新鲜采集合同见 [审计分层](docs/FIELD_AUDIT_LAYERS.md)。历史示例及自动化读取方法见 [不完整采集示例](docs/INCOMPLETE_CAPTURE.md)，其中旧全局 CLONE_FS 回退描述不适用于当前解析器。

The frozen historical Cargo records remain incomplete but browsable: diff=3 is correct for those saved records. Current parsing must not rewrite their evidence. Fresh local-FsContext capture expectations are in the [audit layers](docs/FIELD_AUDIT_LAYERS.md). The [incomplete-capture example](docs/INCOMPLETE_CAPTURE.md) retains obsolete global-CLONE_FS wording as history, which does not describe the current parser.

## Deletion recovery / 删除恢复

Deletion now isolates the complete record under `deleted/TICKET/record` before
physical cleanup. The successful `delete` output/exit stay `Deleted ID`/0;
a post-commit error is 2 and explicitly reports logical deletion plus the ticket,
so failure does not imply no effect. Ordinary list/show/diff read only active
`runs`. Inspect with `deleted list [--json]`, restore an unpurged exact ticket with
`deleted restore TICKET`, or explicitly continue cleanup with `deleted purge TICKET`.
No action is automatic on restart, and a purge-intent marker forbids restore even
if no unlink was observed. Damaged receipts remain visible and are not repaired.
See the bilingual deletion contract in `docs/CRATE_README.md` for commit/sync
boundaries, conflicts and same-UID replacement limitations.

删除先将完整记录隔离到 `deleted/TICKET/record`，然后才物理清理。成功 delete
仍输出 `Deleted ID` 并返回 0；提交后失败返回 2，明确逻辑删除和待检查 ticket，
所以失败不代表没有影响。普通 list/show/diff 只读活动 runs。使用
`deleted list [--json]` 检查；对尚未取得清理许可的精确 ticket 执行
`deleted restore TICKET`；继续清理需显式 `deleted purge TICKET`。重启不自动
恢复/清理；清理意图存在即拒绝 restore，即使未观察到 unlink。损坏收据保留
可见，不自动修复。完整提交/同步、冲突及同 UID 换项边界见双语 crate 说明。

An interrupted signalfd read is an observation error, preserving any previously
latched recorder signal; it is not a completed observation of no pending signal.
Freezing ends acquisition even when that final read fails, so restoration cannot
silently retry it and replace the computed facts. Interrupted finite polls return
to supervision at their existing absolute deadline. An interrupted unlimited
poll yields for another observation without adding a default duration limit.
Continuous successful signal arrivals, individual syscalls and scheduling still
have no unconditional deadline guarantee.

signalfd 读取被中断时返回观察错误并保留已锁存的 recorder 信号，不能把未完成的
观察当作没有 pending 信号。最后读取失败仍结束事实获取；恢复不能悄悄重试并改写
已计算事实。有限 poll 在既有绝对期限到达后返回监督层；无时长限额的 poll 被中断
时仅让出一次观察机会，不新增默认时长限额。连续成功信号流、单个 syscall 与调度
仍没有无条件期限保证。

The private native launcher serves only the two fixed backend argv builders.
Detectable Command cwd/environment edits are rejected before fork. Stdio,
pre_exec callbacks and env_clear have no inspection API; avoiding those edits
remains an audited private call-site constraint, not a general Command adapter.
私有原生启动器只服务两个固定 backend argv 构造器；可检测的 Command cwd/environment
修改在 fork 前拒绝。stdio、pre_exec 与 env_clear 无读取 API，仍要求私有调用点
不作这些修改，不宣称可以替代任意 Command 配置。
