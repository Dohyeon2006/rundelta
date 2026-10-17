# RunDelta

RunDelta is an experimental Linux command execution comparison tool written in
Rust. It uses an external strace executable to record supported execution and
file-access events, then reports observed differences with evidence locations.
Differences are investigation clues, not proof of a root cause.

## Requirements and installation

The tested environment is Linux x86_64, Rust/Cargo 1.98.1 and strace 6.16.
Other architectures, the minimum Rust/strace versions and other distributions
have not been established. Linux pidfd support (normally Linux 5.3 or newer)
and permission to trace child processes are required; a kernel version alone
does not guarantee the necessary permissions.

Install a trusted strace separately. It must support `--kill-on-exit`, `-f`,
`-q`, `-I 2`, `-xx`, `-yy`, `-s` and the syscall selection below. RunDelta does
not bundle strace and never falls back to running the target without tracing.
If strace is outside PATH, set `RUNDELTA_STRACE` to its executable path, without
additional arguments. Privileged or daemonizing tracer wrappers are outside
the supported lifecycle boundary.

From an unpacked source package:

```bash
cargo build --release --locked
./target/release/rundelta --help
cargo install --path . --locked
rundelta --version
```

The executable is `rundelta`; the Cargo package name is `RunDelta`. Add Cargo's
installation `bin` directory to PATH if necessary. A custom installation root
can be supplied with `cargo install --path . --locked --root "$INSTALL_ROOT"`.
These are local source installation instructions, not a claim that a registry
release exists.

## Record and compare

```bash
rundelta record good -- /usr/bin/true
rundelta record repeat -- /usr/bin/true
rundelta diff good repeat
rundelta diff good repeat --json
rundelta list
rundelta list --json
rundelta show good
rundelta show good --json
```

For an investigation, record the same target command in its normal and failing
conditions. Preserve argv and the initial working directory while changing one
explainable condition where possible. Different entry commands can confound a
comparison. Target arguments are passed separately, without implicit shell
expansion. Standard streams are normally inherited, but descriptors 0/1/2
closed before the Rust entry point are not guaranteed to remain closed; see
the runtime boundary below.

Record only explicitly selected startup environment variables with repeatable
`--env NAME`, for example `rundelta record env-check --env PATH -- /usr/bin/true`.
An environment snapshot does not describe all later child environments.

Capture duration and raw-trace size are unlimited by default. Optional soft
boundaries can be enabled explicitly:

```bash
rundelta record bounded \
  --max-duration-ms 30000 \
  --max-trace-bytes 67108864 \
  --terminate-grace-ms 2000 \
  -- /usr/bin/true
```

The duration begins immediately before the formal collector spawn attempt and
excludes the backend version probe. Trace size is sampled from the anchored
inode at intervals of at most about 50 ms and checked again before parsing;
only `observed > limit` triggers. It is therefore a soft boundary that may
overshoot, not an exact quota, and RunDelta does not propagate `RLIMIT_FSIZE`
to the target. A limit sends TERM, waits the configured grace (2 seconds by
default), then uses KILL if needed and reaps the collector. A committed limited
record is `capture_failed`/`incomplete` in default metadata v3 or explicitly
selected trace-v1 metadata v4; record returns 125 and its warning names the limit. A recorder signal observed during grace remains the
primary interrupted status while the limit warning is retained.

超限记录使用默认 metadata v3 或显式 trace-v1 的 metadata v4，状态为
`capture_failed`/`incomplete`，record 返回 125 并保存限制原因；grace 期间观察到
recorder 信号时，中断状态仍优先，限制 warning 保留。

## Opt-in raw evidence and target references / 可选 raw 与目标来源

The default recording protocol remains metadata v3/events v2/marker v1. To retain
trace-local task generations, saved target-exit coordinates, and a marker-bound
receipt of the raw input actually consumed by normalization, opt in explicitly:

默认录制继续使用 metadata v3/events v2/marker v1。需要保存本次 trace 的 task
generation、目标退出坐标和解析实际消费 raw 字节的绑定收据时，显式选择：

```bash
rundelta record traced-a --evidence-contract trace-v1 -- /usr/bin/true
rundelta record traced-b --evidence-contract trace-v1 -- /usr/bin/true
rundelta diff traced-a traced-b --json
rundelta diff good repeat --report-version 5 --json
```

`trace-v1` writes metadata v4/events v3, raw manifest v1 in `raw-integrity.json`,
and finalization marker v2. Diff automatically emits v5 when either successfully
loaded side is metadata v4. `--report-version 5` forces the format even if both
records fail to load. Old-to-old comparisons without this option retain v4.
New-to-old and explicit old-to-old v5 keep the old side's raw/provenance unverified;
they return 3 for an ordinary comparison, rather than upgrading historical facts.

`trace-v1` 写 metadata v4/events v3、`raw-integrity.json` manifest v1 和 marker v2。
成功加载的一侧是 metadata v4 时，diff 自动输出 v5；`--report-version 5` 强制格式，
即使两侧均加载失败也保持 v5。未显式选择的旧记录之间仍输出 v4。新/旧比较或显式
旧/旧 v5 保留旧侧 raw/provenance 未验证事实，普通比较返回 3，不升级历史证据。

Trace-v1 recording supports Linux x86_64. It checks saved target numbers against
its fixed exit-text contract; readers use that contract without host signal-name
lookup. Unknown signal names retain their references with no invented numeric
target result. Historical records keep their original opaque outcome policy.

trace-v1 录制支持 Linux x86_64。保存的目标数值与固定退出文本合同必须一致；
读取不调用主机 signal 名称查询。未知 signal 保留引用，不补造目标数值。历史
记录仍保留原 opaque outcome 政策。固定词汇与 RT 边界见决策文档。

Event indices in saved provenance are zero-based; raw start/end lines are
one-based and inclusive. PID generations describe observed lifetimes within one
trace, not cross-run identity or physical process authentication. Loading validates
saved references without rerunning a newer parser to select a target or backfill
old records. Collector outcomes never substitute for missing target outcomes.

Root-exec and target-exit references must name distinct saved events, with the
exit following the exec in the fixed `(line, end_line, event_index)` evidence
order within `raw/trace.log`; vector indices need not increase. Static metadata
checks still run when events are withheld. Matching each reference to its event
requires the available saved vector.

保存的 provenance 中事件索引从 0 开始；raw 起止行从 1 开始、结束行包含在内。
PID generation 只描述本次 trace 中观察到的生命周期，不是跨 run 身份或物理进程
身份认证。加载验证已保存引用，不运行新 parser 重选目标或为旧记录回填；collector
结果不能代替缺失的 target 结果。

root exec 与 target exit 必须引用不同的保存事件，exit 按 `raw/trace.log` 内固定
`(line, end_line, event_index)` 证据顺序位于 exec 之后；vector 索引不要求递增。
扣留事件时仍执行静态 metadata 检查，引用与实际事件的匹配只在保存 vector 可用时
校验。

For finalized trace-v1 records, complete supported capture requires an exit
observation for every introduced task generation, including a spawned child with
no later actor event. Missing completion must retain a warning and reduced
completeness; otherwise valid incomplete records remain readable and diff returns
3. Every events-v3 exit, including child exits, must use the fixed trace-v1
completion framing: the outcome `success` alone is not an exit observation.
Invalid framing is a load error (diff returns 2). Unknown legal signal names keep
an observed exit without an invented numeric result. Historical metadata v1–v3
and events v1/v2 keep their original interpretation.

已完成结构化提交的 trace-v1 记录要声明支持范围内采集完整，必须为每个已引入的
task generation 保存 exit 观察，包括之后没有 actor 事件的 spawned child。
缺完成观察时必须保留 warning 并降低完整性；其余合同合法的不完整记录仍可读取，
diff 返回 3。每条 events-v3 exit（含 child exit）必须遵守固定 trace-v1 完成
文本格式，单独的 outcome `success` 不能冒充退出观察；非法格式为加载错误，
diff 返回 2。合法未知 signal 名称仍保留退出观察，不补造数值。历史 metadata
v1–v3/events v1/v2 保持原解释，不追溯强化。

Trace-v1 hashes all original normalization input bytes, including line framing,
in the same traversal that feeds the parser. Only successful EOF creates a
`parsed` receipt. Skipped or failed reads save an explained `unverified` receipt.
Finalization rereads saved raw against that receipt before publishing the last
marker; a later hash cannot fabricate parser input provenance. Loading checks raw
bytes, SHA-256 and logical line count separately from structured storage. Hashing
costs work proportional to the trace size; it is not source authentication,
proof of syscall meaning or normalization completeness, a guarantee of power-loss
durability, or an exclusion of external concurrent mutation.

trace-v1 在喂给 parser 的同一次读取中，校验全部原始字节（含换行）；只有成功 EOF
才生成 `parsed` 收据。跳过或失败读取保存带原因的 `unverified` 收据。提交前复读
raw 并与消费收据比对，最后发布 marker；不能靠之后另算 hash 制造解析来源。加载
单独检查 raw 字节、SHA-256 和逻辑行数。校验成本随 trace 大小增长；它不认证来源、
不证明 syscall 意义或归一化完整性、不保证断电持久性，也不排除外部并发修改。

V5 separates capture, structured storage, raw integrity and target provenance.
Missing/changed raw preserves already verified metadata/events facts, but diff
returns 2 with no ordinary comparison conclusion. Show/list may still succeed and
display that independent raw failure. Missing finalization or an explicitly
unverified raw receipt remains 3. Missing/invalid bound manifests and contradictory
version tuples are load errors. Bounded evidence samples remain samples: v5 does
not add an outcome-to-all-evidence mapping or explicit truncation totals.

v5 分开呈现采集、结构化存储、raw 完整性和目标 provenance。raw 缺失或变更保留
已经验证的 metadata/events 事实，但 diff 返回 2、没有普通比较结论。show/list
仍可成功并显示独立 raw 错误。缺完成标记或显式未验证 raw 收据保持 3。已绑定的
manifest 缺失/无效及矛盾版本 tuple 是加载错误。有界 evidence sample 仍是样本；
v5 未增加 outcome 到全部 evidence 的映射或截断总数。

## Full outcome references / 全量 outcome 引用

To inspect every saved occurrence supporting a changed file/exec outcome,
explicitly select report v6:

需要检查某个 file/exec 变化的每条已保存 outcome 证据时，显式选择报告 v6：

```bash
rundelta diff traced-a traced-b --report-version 6 --json
rundelta diff traced-a traced-b --report-version 6
```

V6 adds `evidence_contract="outcome_event_references_v1"` and each change's
`evidence_mapping`. File/exec mappings identify kind/operation/path and group
every original event index and evidence range by outcome. Indices are zero-based
in that run's saved vector; raw lines are one-based and inclusive. Duplicate
occurrences remain distinct even when their coordinates match. Completeness is
limited to saved normalized events for changed keys; unchanged keys and unsupported
raw syscall forms are outside this mapping. Counts, PID/generations and scheduling
order do not become cross-run identity or prove causation/content identity.

v6 增加 `evidence_contract="outcome_event_references_v1"` 与每条 change 的
`evidence_mapping`。file/exec 映射明确 kind/operation/path，并按 outcome 保存
全部原始 event index/证据范围；index 是该 run 保存 vector 的零基索引，raw 行为
一基含尾范围。重复观察即使坐标相同也保留。全量范围仅为变化键的已保存 normalized
events，不含未变化键或未支持的 raw syscall。次数、PID/generation、调度顺序
不成为跨 run 身份，也不证明因果、内容或物理身份。

The familiar flat samples remain capped at three per side. Mapping counts expose
the complete `event_count`, `sample_count`, `sample_event_indices` and
`omitted_sample_count`: the last counts omissions from the small sample, while
the full `outcomes.events` mapping is untruncated. Text keeps the first-five/folding
policy and shows these counts; JSON retains folded changes and all references.
`source` distinguishes normalized events, saved target provenance, metadata and
withheld observations. Metadata field paths remain explicit; metadata/withheld
event and omission counts are null (N/A in text), not invented zero-event facts.
V6 first applies the existing DTO contracts, then excludes every valid
unfinalized side's file/exec events from aggregation, outcome sets and flat/full
references. This also applies to legal nonempty metadata-v3 DTOs constructed by
internal callers; the CLI loader already withholds these events. Such events
cannot introduce a changed key or outcome. Other loaded sides' observations and
metadata comparisons remain browsable. Withheld references are empty and counts
are null, with status 3; malformed DTOs still return 2. Unverified evidence
remains visible. V6 also clears unfinalized target-exit flat samples. V4/v5 keep
their original behavior. Metadata and top-level saved target references remain
visible with unverified provenance quality; they are not finalized mappings.
For target-exit mappings, the singleton outcome is the compared metadata value
(`code`/`signal`/optional raw fallback), rather than the referenced exit event's
raw outcome string. File/exec outcome groups retain the saved event outcome.

旧平面样本仍每侧最多三条。映射显示完整 `event_count`、`sample_count`、
`sample_event_indices` 与 `omitted_sample_count`；最后一项仅计小样本省略，
全量 `outcomes.events` 不截断。文本保留前五和折叠规则并展示数量；JSON 保留
折叠的 change 与全部引用。`source` 区分 normalized events、保存的目标来源、
metadata 和 withheld；metadata 字段路径明确保留。metadata/withheld 的事件数
与省略数为 null（文本 N/A），不伪造零事件。
v6 先执行既有 DTO 合同校验，再统一排除合法 unfinalized 侧的 file/exec 事件，
使其不参与聚合、outcome 集合和任何平面/全量引用。这也覆盖内部调用者构造的合法
非空 metadata-v3 DTO；CLI loader 本来就扣留这些事件。被扣留事件不能产生变化
键或 outcome，其他已加载侧的观察及 metadata 比较仍可浏览。withheld 引用为空、
数量为 null、状态为 3；非法 DTO 仍返回 2，unverified 证据仍展示。v6 也清空
unfinalized 侧目标退出平面样本；v4/v5 原行为不变。metadata 和顶层保存的 target
引用保留，provenance quality 为 unverified，它们不是 finalized 映射。
目标退出映射的单组 outcome 使用 metadata 的比较值（code/signal/必要时 raw
fallback），不使用所引用 exit event 的原始 outcome 字串；file/exec 分组则保持
保存事件的 outcome。

V6 inherits v5 reliability requirements. Ordinary old-to-old or mixed comparison
returns 3 when raw/provenance remains unverified; tool/load/corruption errors
return 2 with no ordinary changes. Even two load failures keep the requested
schema v6 and contract field. Default v4, automatic/explicit v5, recording and
storage formats remain unchanged. References add memory linear in changed saved
events. Diff, show and list JSON stream their complete selected views to a buffered output sink
without allocating the full encoded document; the loaded events, report and full
references remain in memory. List keeps the complete validated index but only
one temporary flat compatibility row while serializing. Text and record summaries still build
owned output strings. Existing storage budgets do not guarantee recovery from arbitrary
memory limits or allocation failure. No new production dependency or raw reparse
is introduced.

v6 沿用 v5 可靠性要求。旧/旧或新/旧普通比较的 raw/provenance 未验证时返回 3；
工具/加载/损坏错误返回 2、无普通 changes。即使两侧都加载失败，仍保持显式 schema
v6 和合同字段。默认 v4、自动/显式 v5、录制及存储格式不变。引用的内存随变化事件
线性增长；diff、show 和 list JSON 将完整所选视图流式写入带缓冲的输出，不分配完整编码文档，
已加载事件、report 和全量引用仍保留在内存中。list 保留完整已验证索引，序列化时仅临时
持有一个平铺兼容项。文本和 record 摘要仍
构建完整输出字符串。既有存储预算不保证任意内存限制或分配失败下恢复。不增加
生产依赖，也不重新解析 raw。

## Exit codes and reliability

For `diff`, text and JSON use the same semantics:

| Code | Meaning |
|---:|---|
| 0 | Complete supported captures and all verification required by the report contract; no observed differences |
| 1 | Complete supported captures and all verification required by the report contract; differences observed |
| 2 | Tool error, missing/unsupported record or corrupt data; no ordinary comparison conclusion |
| 3 | Capture incomplete/unknown or required evidence unverified/unfinalized; observations remain browsable |

V4 requires complete supported captures and verified structured storage for 0/1.
V5 and V6 additionally require verified raw and validated saved target provenance on
both sides. Text and JSON use the same computed status.

v4 的 0/1 要求支持范围内采集完整、结构化存储验证通过；v5/v6 还要求两侧 raw 验证
通过及保存的目标 provenance 验证通过。text/JSON 使用同一计算状态。

JSON separates `observed_differences`, `reliability`, `exit_code` and `errors`.
Corrupt/unreadable comparisons use null for observed differences. Code 3 is not
a verified equality result, even when no differences were observed.

`record` normally preserves the target exit code; target signals return the
numeric value 128 + signal. Without recorder interruption, collector failure
returns 125; initialization and storage errors return 2. Those numbers can overlap with target exit codes, so
inspect metadata and diagnostics. A successful record/show/list command alone
does not certify completeness. List can return 0 while marking damaged rows.
Recorder interruption returns 128+signal without a tool error. Propagated workflow
errors return 2, including signal restoration or non-BrokenPipe summary errors
after commit; formal collector failure facts reduce to an interrupted code or 125.
记录器中断没有工具错误时返回 128+signal。向上传播的工作流错误返回 2，包括
提交后信号恢复或摘要非 BrokenPipe 输出错误；正式采集器失败事实归约为中断码或 125。

## Data, privacy and removal

Data goes to `$XDG_DATA_HOME/rundelta` when set, otherwise
`$HOME/.local/share/rundelta`. Override it with `--storage "$DATA_DIR"`.
Each run stores metadata, normalized events, a finalization marker and raw
strace evidence. Names are labels; records have separate internal IDs.

Unpublished temporary-file cleanup checks that its name still identifies the
opened staging inode. An already absent name needs no removal; a replaced name
observed before unlink is left untouched. Explicit staging aborts retain a failed
cleanup attempt as a secondary diagnostic alongside the primary operation error
and its publication stage. Drop cleanup remains best effort, including a later
retry; a reported failed attempt does not prove the name still exists. The identity observation
and unlink are separate operations, so this is not an atomic exclusion of a
concurrent writer with the same UID. After unlink, cleanup checks the held
staging inode has no links. A surviving link is an error: a late replacement or
an external hard link can cause it, and the check cannot restore removed entries.

未发布临时文件的清理先检查名称是否仍对应已打开的暂存 inode；名称已不存在时
无需删除，unlink 前已发现被替换时保留该叶项。显式暂存中止保留主操作错误及
发布阶段，同时报告清理尝试失败的次要诊断。Drop 清理（包括后续重试）仍为
尽力尝试；已报告的失败不证明名称之后仍存在。身份检查和 unlink
是分开的操作，不能原子排除同一 UID 的并发替换。
unlink 后清理检查仍持有的暂存 inode 是否已无链接；仍有链接会报错。晚到的换项
或外部硬链接都可能造成这个结果，该检查不能恢复已移除的叶项。

The configured storage-root leaf must be a directory, not a symlink, including
when the path has trailing slashes. Ancestor path resolution remains an opening
boundary. Deletion first performs a read-only traversal with a directory-depth
budget of 128 (the selected run root is depth 0), then checks that budget again
while removing entries. A tree already over that budget at preflight returns 2
before removal. Late external changes or I/O errors can still leave a
partially removed run. A post-removal check that the selected directory inode is
still linked reports a deletion error; it cannot restore an already removed
replacement entry. Traversal errors identify whether preflight failed before
removal or removal has started and the record may be partially deleted. The
underlying I/O error remains visible. These checks do not provide transactional deletion.

配置的存储根目录叶项必须是目录，不能是 symlink，带尾随 slash 也不能绕过；
祖先路径解析仍属于打开时的边界。删除前先进行只读遍历，目录深度预算为 128 层
（所选记录根目录为 0 层），删除过程中再次检查预算；预检时已有的超深树在移除前
返回 2。晚到的外部变更或 I/O 错误仍可能
留下部分删除的记录。移除后若所选目录 inode 仍有链接，会报告删除错误，但不能
恢复已经移除的替代叶项。遍历错误区分预检在移除前失败与移除已开始、记录可能
部分删除，并保留底层 I/O 错误；这些检查不提供事务性删除。

Index scans keep only a bounded number of record descriptors open. If a scan
cannot inspect an entry because of an I/O error, recording fails before
reserving a label or starting the target; unknown labels are never treated as
available. 索引扫描只同时持有有界数量的记录描述符；扫描发生 I/O 错误时，
录制会在预留标签或启动目标前失败，不会把未能读取的标签视为空闲。

Structured storage uses explicit operation budgets on both writes and reads:
metadata 8 MiB, finalization marker and opt-in raw manifest 64 KiB each,
each JSONL line 8 MiB including LF, events file 256 MiB, at most 1,000,000 events, and at most 65,536 JSON values
(keys, strings, containers and scalars) per JSON document. These are resource
limits, not schema corruption. Exceeding a budget reports `unavailable`;
list retains healthy rows and show/diff return 2 for the affected record.
Raw capture duration/size remain governed by the separate optional soft limits.
Finite JSON allocations remain; recovery under every possible memory limit is
not guaranteed. Current file/exec events require an observed path and valid
family/operation shape; checksums alone cannot establish these facts.

结构化存储读写采用相同操作预算：metadata 为 8 MiB，完成标记及可选 raw manifest
各为 64 KiB，JSONL 单行（含换行）为 8 MiB，events 文件为 256 MiB，事件最多 1,000,000 条，
每份 JSON 的键、字符串、容器和标量合计最多 65,536 项。超限归类为
`unavailable`；list 保留健康记录，受影响记录的 show/diff 返回 2。预算与 raw
采集的可选软上限独立，不能保证任意内存限制下均可恢复。当前 file/exec 事件
必须有已观察的路径，事件家族与操作必须匹配；checksum 不代替这些语义校验。

The binary `record` entry reexecutes its current `/proc/self/exe` inode with the same PID before acquiring capture resources. This destroys old host threads/reapers and resets caught signal handlers according to exec semantics. A sealed anonymous descriptor transports raw argv, policy and the original environment list; owner startup alone omits `LD_*` and `GLIBC_TUNABLES`. A private, single-threaded environment guard restores all original native bytes, duplicate/malformed entries and order before starting the backend/target. Target argv, cwd, standard IO, PID and session remain inherited; existing Rust-startup closed-descriptor/SIGPIPE boundaries still apply. This supersedes the old entry-only thread/SIGCHLD refusal; the fresh owner exclusively supervises its children. The private selector is not a public API or authentication boundary, and no returning multithreaded embedding API is supplied.

`record` 在获取采集资源前，以相同 PID 重执行当前 `/proc/self/exe` inode，销毁旧宿主线程/reaper，按 exec 语义重置 caught handler。密封匿名 FD 传入原生 argv、策略和原始 environment 列表；只为所有者启动移除 `LD_*`、`GLIBC_TUNABLES`。单线程独占 environment guard 在后端/target 启动前恢复完整原生字节、重复/非法条目及顺序。目标 argv、cwd、标准 IO、PID 和会话继承；既有 Rust startup 关闭槽/SIGPIPE 边界仍适用；新边界取代旧入口线程/SIGCHLD 拒绝。私有 selector 不是公共 API 或认证，不提供可返回的多线程 embedding 接口。

Caught HUP/INT/TERM/SIGCHLD callbacks in the old image cannot be restored or invoked. Default/ignored dispositions and the complete mask retain platform cross-exec semantics. The owner temporarily establishes waitable SIGCHLD and passes the inheritable state to its children. A post-freeze signal previously handled by an old callback can now terminate the CLI by default without changing committed capture facts. Guards cannot move/share between threads, and the current owner callgraph neither creates host threads nor changes environment or loads old hooks. The raw snapshot still assumes no concurrent native environ mutation; hostile FFI/ptrace and unconditional kernel deadlines are not established capabilities. The raw frame budget derives from the existing kernel argv/environment budget and adds no output or evidence quota.

旧镜像 caught HUP/INT/TERM/SIGCHLD callback 不再执行，也不能恢复旧函数地址。默认/忽略 disposition 与完整 mask 保持平台跨 exec 语义。所有者临时规范化 SIGCHLD 以回收 child，并向 child 传入可继承的原状态。事实冻结后到来的信号过去可能调用旧 callback，现在可默认终止 CLI，而不会改写已提交采集事实。guard 不可跨线程移动/共享，当前 owner 调用图不创建宿主线程、不改 environment、不加载旧 hook。原快照仍假设没有并发 native environment 改写；恶意 FFI/ptrace 和无条件内核期限没有建立。原生 frame 上界由既有内核 argv/environment 预算推导，不新增输出/证据配额。

Arguments, paths and selected environment values may contain secrets. They
are stored and displayed without automatic redaction. Hex escaping is
reversible, not encryption. Review both reports and raw records before sharing.
Run directories are created with mode 0700; this does not sanitize their content.

```bash
rundelta delete good
cargo uninstall RunDelta
```

Delete permanently removes that run and its evidence without an interactive
confirmation. Use a safe directory ID from list when a damaged record's name
cannot be recovered. Uninstall removes the executable, **not recorded data**.
If installed using `--root`, supply the same root to `cargo uninstall`.

## Observation and compatibility boundaries

The supported trace selection is `execve`, `execveat`, `open`, `openat`,
`openat2`, `access`, `faccessat`, `faccessat2`, `chdir`, `fchdir`, `clone`,
`clone3`, `fork`, `vfork`, `unshare`, `setns`, `chroot`, `pivot_root`, `mount`,
`umount2`, `move_mount` and `mount_setattr`, with process exits and
unfinished/resumed evidence. Support is limited to the documented event forms,
not all Linux process, mount or namespace behavior.

Absolute spellings come from syscall arguments or decodable strace path/fd
annotations. The last observed CWD is context, not proof of a current physical
path. Relative access without sufficient evidence stays uncertain. External
untraced directory renames cannot be reconstructed. Proven CLONE_FS sharing is
modeled per filesystem context; unknown flags, transitions or task identity
relationships remain incomplete. No assumption is made that every thread or
child shares the same context.

RunDelta restores the caller's HUP/INT/TERM dispositions and complete signal
mask at the target `pre_exec` boundary. It does not promise strict
direct-versus-record equivalence for `SIGPIPE` ignored before Rust startup or
for descriptors 0/1/2 already closed at that boundary: the Rust runtime or
launcher path may have changed them before RunDelta can observe them. Closing
that gap would require a separately reviewed native pre-runtime shim.

Capture facts freeze before structured finalization. HUP/INT/TERM arriving after
that boundary stay pending while the recorder attempts to restore its original
dispositions and mask, on both commit success and failure. A default late signal may therefore
terminate the recorder even though target completion was already recorded; it
does not become the target's signal or change finalized metadata.
结构化提交前会冻结采集事实。此后收到的 HUP/INT/TERM 保留为 pending，成功或失败
清理时尝试按 recorder 原有信号处理方式和 mask 恢复。默认处理可能终止 recorder，
即使目标已完成；不会把迟到信号冒充为目标信号，也不会回写已完成的 metadata。

A restoration failure returns tool error 2 and preserves the ID if the record
was already committed. Drop retries once as best effort; persistent syscall
refusal can leave recorder controls blocked. Neither running Drop nor returning
the error proves restoration succeeded, and the failure does not rewrite target,
collector or capture facts. Constructor rollback has the same best-effort limit.

信号恢复失败返回工具错误 2；记录已经提交时，诊断保留其 ID。Drop 会尽力再试一次，
持续的 syscall 拒绝仍可能让 recorder 控制信号保持阻塞。执行 Drop 或返回错误
不能证明恢复成功，也不改写目标、采集器或 capture 事实；构造回滚同样为尽力尝试。

The pre-recording backend probe starts its existing five-second clock before
private fork. Native argv and launch descriptors are prepared first; the parent
owns the waitable child and pidfd before polling its CLOEXEC exec-error pipe,
signalfd, pidfd and bounded stdout/stderr. The child restores copied signal
state after arming PDEATHSIG and checking its parent, then uses libc execvp
with the original raw environment. This preserves the current PATH/ENOEXEC
behavior and explicitly retains Command's backend SIGPIPE-default rule.
Version streams retain their existing 64-KiB limit and 4-KiB diagnostic view;
nonblocking final drains never wait for descendant pipe EOF.

版本探测的既有 5 秒时钟从私有 fork 前开始。父进程先持有 waitable child/pidfd，
再共同轮询 exec-error 管道、信号、进程与有限输出。child 在恢复原信号前设置
PDEATHSIG 并检查父 PID，随后以原 raw environment 调用 libc execvp；保留当前
PATH/ENOEXEC 及 backend SIGPIPE 默认处理规则。非阻塞排空不等待后代管道 EOF。

Formal startup uses the existing duration, byte observations, signal forwarding
and termination grace while its handshake is pending. An observed recorder
HUP/INT/TERM inherited as blocked or ignored cancels the collector directly;
the guard terminates it with SIGKILL and reaps it. Only other inherited states
forward the signal and use grace supervision. A successfully reaped
limited launch retains the original capture_failed/incomplete facts and exit
125; limits and recorder cancellation are not converted into a generic startup
error. Default formal capture still has no arbitrary duration or output quota.

正式握手等待期间仍观察既有时长/raw 大小、信号和 TERM grace。观察到的记录器
HUP/INT/TERM 若继承为屏蔽或忽略，则直接取消 collector，由 guard 通过 SIGKILL
终止并回收；其他情况才转发信号并进行宽限监督。成功回收的超限
启动保留 capture_failed/incomplete、限制警告及退出 125，不被改成通用启动错误。
默认正式采集不新增任意时长或输出配额。

Each explicit reap uses WNOHANG and an independent five-second userspace
budget. Failed normal reap can be followed by another cleanup budget, so this
is not a global five-second deadline. KILL acceptance and pidfd readiness do
not establish a reap. If waiting loses authority or cannot complete, the guard
revokes numeric cleanup before inherited SIGCHLD is restored. A typed
unquiesced error returns tool2: preflight creates no record; formal capture
retains initial running metadata/raw at the reported ID/directory, without
reading, normalizing, freezing or finalizing possibly changing evidence. Do not
rerun the target automatically. Existing committed-but-restoration-failed
results remain separate. Drop attempts KILL and one nonblocking wait only.

每次显式回收使用 WNOHANG 与独立 5 秒用户态预算；普通回收失败后清理可再使用
一份预算，不能宣称全局 5 秒。KILL/pidfd 就绪不等于回收。未完成或失去 wait 权限
时，在恢复 SIGCHLD 前撤销数字 PID 清理权；类型化错误返回工具 2。版本探测不
建记录；正式采集保留诊断中的 ID/目录及 running/raw，不读取、归一化、冻结或
定稿可能变化的证据，不自动重跑。已提交但恢复失败仍独立表达；Drop 仅尽力 KILL
和一次非阻塞 wait。单个 fork/exec/pidfd/信号/wait/poll 调用及调度仍无无条件
内核期限，PDEATHSIG 不把这些限制变成硬期限。

Before either supervised spawn, the admitted fresh owner requires SIGCHLD=SIG_DFL without SA_NOCLDWAIT. Its lifecycle scope temporarily normalizes the inherited default/ignored state and restores it explicitly after fact freezing, commit and control-signal restoration; a restoration failure retains a committed record ID independently. The child receives the original inheritable SIGCHLD state before exec. Once pidfd_open reports ESRCH or waiting reports ECHILD, cleanup never signals an untrusted numeric PID.
监督 spawn 前，新镜像 owner 要求 SIGCHLD 为默认处理且不带 SA_NOCLDWAIT；独占生命周期 scope 临时规范化可继承的默认/忽略状态，在事实冻结、提交和控制信号恢复之后显式恢复。恢复失败独立保留已提交记录 ID。child 在 exec 前恢复原可继承 SIGCHLD。失去 child 身份后，清理不向不可信数字 PID 发信号。

Default records use metadata v3/events v2/marker v1; default old-to-old diff JSON
uses v4. Metadata v1/v2 and events v1 remain readable. Legacy records without
checksums are unverified, not automatically corrupt or upgraded. Missing current
finalization is unfinalized. Marker v1 verifies metadata/events byte counts,
event count and SHA-256, without checksumming raw. Explicit trace-v1 uses the new
versions described above; marker v2 also binds the raw receipt manifest. Neither
marker authenticates origin, prevents deliberate rewrites or guarantees power-loss
durability. Old readers reject the new versions rather than silently promoting
records. Finalized byte checks precede schema/DTO interpretation. Events are
written and validated incrementally; old-tuple list checks facts in validate-only
mode without retaining the complete vector. Trace-v1 marker-v2 listing temporarily
collects one record's events to validate saved generations/provenance, then
discards that vector when constructing its summary; returned list rows contain
no events. Show/diff retain events in the loaded run. Existing per-record storage
budgets apply, without guaranteeing recovery from arbitrary allocation failure;
the index and summaries still grow with the number of records.

默认录制及旧/旧比较继续使用 metadata v3/events v2/marker v1/diff v4。metadata
v1/v2、events v1 仍可读；未验证旧记录不自动归为损坏，也不升级。当前记录缺完成
标记为 unfinalized。marker v1 验证 metadata/events 的字节数、事件数与 SHA-256，
不校验 raw；显式 trace-v1 使用上面的新版本，marker v2 还绑定 raw 收据 manifest。
两种 marker 都不认证来源、不阻止蓄意改写、不保证断电持久性。旧读者拒绝新版本，
不能默默加强其证据。完成文件的字节校验先于 schema/DTO 解释；events 增量写入与
验证。旧 tuple 的 list 以 validate-only 模式检查事实，不保留完整 event vector；
trace-v1 marker-v2 列表路径临时收集一条记录的事件，以验证保存的 generation/
provenance，构造 summary 时释放 vector，最终列表行不保留事件。show/diff 的
loaded run 则保留事件。现有逐记录存储预算仍适用，但不保证任意分配失败下恢复；
索引与 summaries 总量仍随记录数增长。

Marker rename can succeed before directory sync fails;
an error can therefore coexist with a visible marker. SIGKILL leaves actual
persisted state for normal validation, not an assumed running/pending state.
标记 rename 后的目录同步仍可能失败，工具错误可以与可见标记并存。SIGKILL
之后按实际落盘文件正常校验，不能一概推定 running/pending 或标记缺失。

CMake/Cargo and other dynamic builds can produce substantial normal-repeat
noise. A capture may be incomplete and correctly produce diff=3 while still
showing useful execution/file-access clues. No top-five ranking guarantee is
made. Same paths do not prove same contents, opening a file does not prove that
its contents affected execution, and the tool does not diagnose arbitrary file
content differences. It offers no general root-cause determination, automatic
repair, complete replay, network-content capture or retrospective recovery of
unrecorded runs. Tracing can affect timing.

The crate is a build/install source distribution. Internal acceptance reports,
external-case evidence, probes and development regression fixtures are deliberately
excluded. Run the full test suite in the development checkout; `cargo test` in
the reduced package is not supported because frozen fixture files and a private
developer test module are absent. The developer test module is intentionally
outside the public Cargo allowlist; the payload and privacy budget are unchanged.

精简 package 用于构建和安装；完整测试在开发 checkout 运行。package 刻意排除冻结
fixture 及私人开发测试模块，因此不支持其中的 `cargo test`。私人测试不加入 public
Cargo allowlist，发布内容与隐私预算不变。

## License

RunDelta is licensed under the MIT License; see [LICENSE](../LICENSE).
Copyright (c) 2026 The RunDelta contributors.
Dependencies and the separately installed strace retain their own licenses.

### Explicit deletion isolation and recovery / 显式删除隔离与恢复

`delete ID_OR_LABEL` keeps its successful exit 0 and `Deleted ID` output only
when both the selected record and its bookkeeping have been physically removed.
It now first moves the whole directory from `runs/ID` to
`deleted/TICKET/record` on the same filesystem. That rename is the **logical
deletion commit**, before the first evidence unlink. Ordinary list/show/diff only
read `runs`; a partial cleanup cannot appear as a partially deleted active run.
Old storage remains readable without a migration, and no saved schema, event,
coordinate, provenance or finalization marker is rewritten.

`delete ID_OR_LABEL` 仍只在记录和本次隔离账目均物理删除完成后返回 0 和
`Deleted ID`。现在先在同一文件系统内将整个目录由 `runs/ID` 移到
`deleted/TICKET/record`；该 rename 是**逻辑删除提交点**，先于任何证据 unlink。
普通 list/show/diff 只读 runs，因此部分清理不会变成活动区中的半条记录。
旧存储无需迁移；原 schema、事件、坐标、provenance 和 finalization marker 不改写。

An error before rename returns 2 without this operation unlinking evidence. This
preservation statement assumes the selected inode is not concurrently modified
by a writer ignoring the lock. A post-rename identity anomaly returns 2 with
**result unknown**, stops cleanup and retains the slot. A sync or cleanup error
after the verified rename returns 2 explaining that logical deletion committed,
the ticket to inspect, and the possibility of partial physical cleanup. Exit 2
therefore is **not** a promise that deletion had no effect. No automatic rollback,
restart restore or restart purge occurs. Directory sync failure does not erase
the visibility change or establish power-loss durability.

rename 前失败返回 2，本操作尚未 unlink 证据；完整性前提是选中 inode 没有被
不遵守锁的写入者并发改动。rename 后身份异常返回 2、明确结果未知并停止清理，
保留隔离项。已核验 rename 后的同步或清理失败返回 2，明确逻辑删除已提交、
待检查 ticket 和可能部分物理清理。2 **不承诺没有产生删除影响**。没有自动回滚、
重启恢复或重启清理；目录 sync 失败不抹去可见性变化，也不建立断电耐久。

```text
rundelta deleted list [--json]
rundelta deleted restore TICKET
rundelta deleted purge TICKET
```

Recovery accepts one exact ticket ID, never a label or path. The private v1
receipt binds the original ID, observed label and live device/inode identities;
it does not authenticate origin. Listing isolates damaged slots and reports
`isolated`, `purge_started`, `no_record` or `damaged`. Missing payload alone does
not prove whether preparation, purge or restore previously completed. Restore
requires an intact selected payload and **no** purge-intent marker; it refuses
active ID/known-label conflicts and uses a no-overwrite rename. It preserves any
original record damage rather than manufacturing a marker. After restore's
rename, sync/bookkeeping errors explicitly say the active record was restored.

恢复只接受精确 ticket ID，不接受标签或路径。私有 v1 收据记录原 ID、观察到的
标签及 live device/inode 身份，不认证来源。列表隔离损坏项并报告 isolated、
purge_started、no_record 或 damaged；payload 缺失本身不能证明此前准备、清理
或恢复是否完成。restore 要求 payload 身份匹配且**没有**清理意图标记，拒绝
活动 ID/已知标签冲突，使用不可覆盖 rename，保留原有损坏，不制造 marker。
恢复 rename 后同步或账目清理失败，诊断明确活动记录已经恢复。

Purge writes and syncs its intent before unlink; marker write/sync failure stops
before evidence removal. Intent permits cleanup, but does not prove an unlink
occurred. Conservatively, restore refuses even when a marker exists but no bytes
were removed. A failed purge leaves an enumerable receipt and remaining payload
unless unrelated same-UID interference damages them; continuing purge is an
explicit choice. A malformed receipt/intent is never silently repaired.

purge 在 unlink 前写入并同步清理意图，写入/同步失败即停止证据删除。意图允许
清理，不证明已发生 unlink；为避免把部分数据当完整数据，存在意图即保守拒绝
restore，即使尚未删除任何字节。失败 purge 保留可枚举收据和剩余 payload，
前提是没有外部同 UID 干扰损坏它们；继续 purge 必须显式选择。损坏收据或意图
不会被静默修复。

The cooperative lock serializes RunDelta mutations. Directory FDs anchor I/O,
identity checks detect observed replacement. Isolation directories must be owned
by the effective UID with no group/other permissions; unsafe existing permissions
are rejected without chmod. This excludes ordinary other-UID access.
None prevents a noncooperating same-UID writer from swapping a name between a
check and rename/unlink, forging receipts or changing evidence. Detection is not
rollback and cannot undo partial removal. An effective hostile same-UID boundary
requires a separately configured service identity/permission boundary and an
authorized IPC protocol; this CLI does not reconfigure system security. Legacy
kernels lacking no-overwrite rename fail closed instead of replacing a target.

协作锁串行化 RunDelta 写操作；目录 FD 锚定 I/O，身份核验检测观察到的换项，
隔离目录必须由有效 UID 拥有且无 group/other 权限，已有非私有目录直接拒绝而不
chmod；这限制普通其他 UID 的访问。它们不能防御同 UID 非协作进程在核验与 rename/unlink 间
换名、伪造收据或改证据。检测不等于回滚，不能撤销部分删除。抗恶意同 UID 的
有效边界需要另行配置独立服务身份/权限及授权 IPC；本 CLI 不改变系统安全策略。
不支持不可覆盖 rename 的旧内核直接报错，不回退到会覆盖对象的 rename。

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
