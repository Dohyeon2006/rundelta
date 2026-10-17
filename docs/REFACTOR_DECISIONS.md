> 本地证据路径说明：本文及本索引所链接历史文档中的 `target/`、本机实验目录和未发布原始日志路径，仅用于说明原本地复现实验位置；这些产物不随公开版提供，不是可访问的公开证据链接。公开可核验材料以仓库实际收录的文件和公开派生合同为准，不能据本地路径推断已发布原始记录。
> Local evidence paths: `target/`, local experiment directories and unpublished logs describe original local reproductions. Their artifacts are not shipped with this public edition and are not accessible public evidence links.

# 重构产品决策 / Refactor product decisions (2026-09-21)

Status: DEC-01–05 accepted for the compatibility refactor; A18 trace-v1 was verified on source tree `c092a500f9be2aece76ded3bede19f797c937efd`. The explicit v6 extension below is implemented for candidate acceptance; its final result requires its own fixed-source receipt.

状态：DEC-01～05 已接受，用于兼容性重构；A18 trace-v1 已在源码树 `c092a500f9be2aece76ded3bede19f797c937efd` 验收。下文显式 v6 扩展已实施、进入候选验收，最终结果须来自自己的固定源码凭据。下列中英合同表达同一决策；发生歧义时应通过测试和相应 schema 合同确认，不从历史自由文本反推新语义。

These decisions resolve DEC-01 through DEC-05 from the full refactor task
list. They preserve the existing command and storage contracts unless a user
explicitly opts into a capture limit, A18 trace-v1 or the v6 report. They do not
strengthen the meaning of observed evidence.

## DEC-01: capture resource limits

中文摘要：默认仍不限制录制时长和 raw trace 大小，只有调用者显式给出时才启用；文件大小轮询是软边界。终止 grace 默认 2 秒，超限保存为默认 metadata v3 或显式 trace-v1 的 metadata v4，状态为 `capture_failed`/`incomplete`，带明确 warning；record 返回 125。

Capture remains unlimited by default for compatibility. The refactor may add
explicit duration and raw-trace byte limits. A limit is active only when the
caller supplies it; documentation must call file-size polling a soft boundary,
not an exact byte guarantee. Supervised termination uses a two-second grace
period by default before escalation to `SIGKILL`.

When an explicit limit is reached, the collector and target tree must be
terminated and reaped, capture completeness must be `incomplete`, the warning
must identify the stop reason, and the record command must return 125. The default metadata writer remains v3; opt-in
trace-v1 uses v4. Both retain
the limit reason in existing warnings rather than introducing a structured
recorder-outcome field.

## DEC-02: cancellation semantics

中文摘要：取消采用 best effort，不承诺收到取消后目标绝无副作用，本周期不引入 exec gate。只能描述监督器首个观察到的信号，不能声称普通信号的真实发送顺序。

Recorder cancellation is best effort: supervision starts as early as the
current Rust process boundary permits and stops the collector tree promptly,
but the product does not promise that the target has performed zero side
effects. No exec-gate helper is introduced.

The supervisor may describe the first signal it observed; it must not claim
the kernel's true send order for ordinary coalescing signals. Existing metadata
v3 signal/state fields and exit-code behavior remain the compatibility surface.

The final observation before structured finalization freezes capture facts. Later
HUP/INT/TERM signals belong to the recorder's original signal semantics: do not
read them out of signalfd during finalization or restoration retries. Restore
saved dispositions before the saved mask, leaving a blocked signal pending and
allowing the original default/ignored action to apply. This remains true if
storage finalization fails. Never relabel these signals as target completion or
forward them to a former collector/parent process.
结构化提交前的最终观察冻结采集事实；迟到信号不再由 signalfd 消费。先恢复原
disposition，再恢复原 mask，保留原有默认、忽略或阻塞语义；提交失败和恢复重试
也遵守此边界。迟到信号不是目标结局，不能转发给已结束的 collector 或调用父进程。

## DEC-03: recorder outcome persistence

中文摘要：默认继续写 metadata v3，A18 显式选择 trace-v1 时写 v4；signal、timeout、trace limit 与 supervision failure 作为彼此独立的内存事实归约到现有字段、warning 和 CLI 状态，不增加结构化 `RecorderOutcome`，也不改写历史记录。

Default records continue to use metadata v3. The A18 trace-v1 extension writes
metadata v4 only when explicitly selected. Selected/additional recorder signals,
timeouts, trace limits, and supervision failures are not added as a structured
`RecorderOutcome` in this cycle. Relevant failures remain separate in memory
and use existing state, completeness, warning, and CLI status channels without
pretending that free-form historical warnings are structured facts.

A18 metadata v4 adds saved target references, not a structured RecorderOutcome.
No historical record is migrated, rewritten or retrospectively supplied with
provenance.

## DEC-04: pre-runtime transparency

中文摘要：不承诺继承的 SIGPIPE disposition 或 Rust 启动前已经关闭的 fd 0/1/2 与直接执行严格等价；以特征测试和文档冻结此边界，本周期不加入 native pre-runtime shim。

The current Rust entry point does not promise strict direct-versus-record
equivalence for an inherited `SIGPIPE` disposition or for file descriptors
0/1/2 that were already closed before the Rust runtime began. This boundary is
documented and covered by characterization tests. A native pre-runtime shim is
not introduced in this cycle.

Ordinary target standard streams remain inherited, and RunDelta's own output
handling must not be described as changing the target's `SIGPIPE` semantics.

## DEC-05: diff evidence format

中文摘要：默认旧记录比较保持 schema v4 和有界平面 evidence sample；优先抽样发生变化的 outcome，不暗示 sample 包含全部证据。A18 的 v5 只补独立 raw/provenance 合同；全量映射及样本省略数由下文显式 v6 扩展承接，不回写 v4/v5。文本折叠不得删除 JSON change 或改变可靠性/退出码。

Default old-record comparisons remain schema v4 with bounded, flat evidence
samples. Prefer changed outcomes before using remaining capacity for common
outcomes. The bounded samples must not be presented as all evidence. A18 v5 adds
independent raw/provenance facts only; it does not add an outcome-to-all-evidence
mapping or explicit truncation totals. The separately versioned v6 contract below
implements that extension without altering the frozen v4/v5 views.

The outcome-to-evidence mapping and sample omission totals use the explicit,
separately reviewed format extension below. Text folding may change presentation only;
it must not remove JSON changes or alter reliability and exit-code semantics.

## A18：可选 trace-v1 合同 / Opt-in trace-v1 contract

状态：2026-10-07 已实施并在上述 c092 源码树验收；本地原始凭据见
A18 整改账（原本地路径 `../target/a18-remediation/20261007/remediation.json`）。本节记录合同，
后续扩展不能借用 A18 的验收结果。
Status: implemented and verified on 2026-10-07 at the c092 source tree above;
the local original receipts are indexed by the
A18 ledger（原本地路径 `../target/a18-remediation/20261007/remediation.json`）. This section
records the contract; later extensions cannot borrow A18's acceptance result.

### 版本与显式选择 / Versions and explicit selection

默认录制继续写 metadata v3、events v2、marker v1，默认旧记录之间的 diff 仍为 v4。
`record --evidence-contract trace-v1` 显式写 metadata v4、events v3、
`raw-integrity.json` manifest v1 和 marker v2。新事件有 trace-local generation；
新 metadata 保存目标选择与退出的 provenance。marker v2 用 `raw_manifest` 摘要
绑定 manifest 原始字节，并继续绑定 metadata/events。无绑定的可选 sidecar 是
最简单替代，但缺失、替换和旧读者的可信判定没有版本边界，不能建立新承诺，故未采用。

Default recording keeps metadata v3/events v2/marker v1; default old-to-old diff
keeps v4. `record --evidence-contract trace-v1` explicitly selects metadata v4,
events v3, raw manifest v1 in `raw-integrity.json`, and marker v2. New events carry
trace-local generations; metadata saves target-selection and exit provenance.
Marker v2 binds the manifest's original bytes through `raw_manifest`, alongside
metadata/events. An unbound optional sidecar is the simplest alternative, but it
cannot establish the new contract across missing/replaced files and old readers'
trust classifications, so it is not used.

只要成功加载的一侧是 metadata v4，diff 自动选 v5。`diff --report-version 5`
强制选择 v5，即使两侧加载都失败也保持 v5 格式。new/old 比较或显式 old/old v5
不升级旧记录：旧侧 raw/provenance 为未验证，普通比较返回 3。默认 old/old v4
保留原合同。v5 的 0/1 需要两侧采集完整、结构化存储验证通过、raw 验证通过及
保存的目标 provenance 验证通过。2 是工具/加载/损坏错误，没有普通比较结论；
3 是不完整或未验证证据。text/JSON 消费同一计算结果。

Diff selects v5 when either successfully loaded side is metadata v4.
`diff --report-version 5` forces v5 even when neither side loads. New-to-old or
explicit old-to-old v5 does not upgrade the old side: raw/provenance remain
unverified and an ordinary comparison returns 3. Default old-to-old v4 retains
its existing contract. V5 status 0/1 requires complete capture, verified
structured storage, verified raw, and validated saved target provenance on both
sides. Status 2 is a tool/load/corruption error without an ordinary conclusion;
3 means incomplete or unverified evidence. Text/JSON use the same computed result.

### 来源与独立事实 / References and independent facts

provenance 从本次解析的选择结果复制，保存选择/身份合同、观察到的 PID、generation、
root exec 与 target exit 的事件索引和原始 evidence 坐标。事件索引从 0 开始；raw
start/end 行从 1 开始、结束行包含在内。generation 仅描述一次 trace 内数字 PID
的已观察生命周期，不能成为跨 run 比较身份或物理进程身份认证。加载只验证保存的
DTO 一致性，不运行新 parser 重选目标或给旧记录回填。未知信号、缺失 root exit、
collector 结果、采集失败及存储事实仍独立，禁止用 collector 填造 target。

Capture copies provenance from this parse's selected observations, retaining the
selection/identity contracts, observed PID/generation, root-exec and target-exit
event indices, and original evidence coordinates. Event indices are zero-based;
raw start/end lines are one-based and inclusive. A generation describes an
observed numeric-PID lifetime within one trace; it is neither cross-run comparison
identity nor physical process authentication. Loading validates saved DTO
consistency without running a newer parser to reselect or backfill records.
Unknown signals, missing root exits, collector outcomes, capture failures and
storage facts remain separate; the collector must never stand in for the target.

只有 trace-v1 在实际喂给解析器的同一次读取中累计全部原始字节（含换行）的 SHA-256、
字节数与逻辑行数，成功 EOF 才保存 `parsed` 收据。跳过、UTF-8/消费者/读取失败只能
保存带原因的 `unverified` 收据，不能在之后另读并制造解析来源。finalization 复读
raw，比对原消费收据，再最后发布 marker；加载独立复核 raw 的长度、SHA-256 与
逻辑行数。需要这些 I/O 才能检查当前保存字节是否对应消费收据；它不验证 syscall
意义或归一化完整性，不认证来源、不证明因果关系、不保证断电持久性，也不构成阻止
外部同权限修改的事务。

Only trace-v1 hashes all original bytes, including framing, in the same traversal
that feeds normalization. Successful EOF saves a `parsed` receipt with SHA-256,
byte count and logical line count. Skipped or UTF-8/consumer/read failures save
only an explained `unverified` receipt; a later read cannot fabricate input
provenance. Finalization rereads raw against the consumption receipt before
publishing the last marker. Loading independently checks raw length, SHA-256 and
logical lines. These I/O operations are needed to compare saved bytes with that
receipt. They do not validate syscall meaning or normalization completeness,
authenticate origin, prove causation, guarantee power-loss durability, or prevent
external same-UID mutation transactionally.

raw 缺失、截断、同长改字节等损坏保留已验证的 metadata/events 事实，载入独立
`raw_integrity=corrupt`；访问失败为 `unavailable`。diff 将两者归为 2，并清空普通
change/observed-difference 结论。缺 marker 是 unfinalized；显式未验证收据保持 3。
已有 marker v2 却缺 manifest、manifest 摘要/合同错误及错误版本 tuple 是加载错误。
旧 tuple 不检查或升级 raw，也不读取可选 sidecar。

Missing, truncated or same-length changed raw retains verified metadata/events
facts and loads an independent `raw_integrity=corrupt`; access failures are
`unavailable`. Diff returns 2 for either, without ordinary changes or an observed
difference conclusion. Missing marker is unfinalized; an explicit unverified
receipt remains status 3. A present marker v2 with a missing manifest, invalid
manifest bytes/contract, or contradictory version tuple is a load error. Old
tuples never inspect or upgrade raw and do not read optional sidecars.

保存的 identity v1 还冻结最低不确定性：first-successful exec 及同代 exit
存在时不能删除 target provenance/raw outcome；未由 spawn 引入的新 actor、
退出后重新出现和 live collision 的 actor 必须保留 context_uncertain，记录
保留 warning 且不能声称 complete；发生 collision 的 spawn 行也保留不确定性。
校验不固定 warning 文案或重推 FS sharing。

Identity v1 also freezes minimum uncertainty: a saved first-successful exec and
same-generation exit require target provenance/raw outcome. Unintroduced,
reappeared and live-collision actors retain context_uncertain; the record retains
a warning and cannot claim complete capture. A colliding spawn row also retains
uncertainty. Validation does not fix warning text
or reconstruct filesystem sharing.

### 存储边界与验收输入 / Storage boundaries and acceptance inputs

当前 reader 的错误优先级固定为：marker header/DTO/record ID；metadata、events、
manifest 的全部绑定字节检查；metadata 复读字节检查与解码；events 的流式字节检查、
解码、数量与完整领域合同；版本 tuple；manifest header/DTO/跨 metadata 合同；
最后是独立 raw 校验事实。旧 tuple 的 list 继续 validate-only 流式行为；trace-v1
marker-v2 分支临时收集一条记录的 event vector，使用与完整加载相同的纯 generation/
provenance 校验。构造 summary 时释放该 vector，返回的列表行不保留事件。
现有逐记录资源预算仍适用；索引和 summary 总量随记录数增长，不保证任意分配
失败下恢复。替代方案是为 EventFacts 增加按索引选择的额外状态，会增加阶段耦合，
当前未引入。

The current reader freezes this priority: marker header/DTO/record ID; all bound
metadata/events/manifest byte checks; metadata reread check and decode; event
stream byte check, decode, counts and full domain contract; schema tuple; manifest
header/DTO/cross-metadata contract; independent raw verification facts. Old tuples
keep validate-only streaming. The trace-v1 marker-v2 listing path temporarily
collects one record's event vector for the same pure generation/provenance
validation as a full load, then discards it during summary construction; returned
rows contain no events. Existing per-record resource budgets apply; index and
summary totals grow with record count and arbitrary allocation recovery is not
guaranteed. An alternative would add index-selected state to EventFacts and
increase coupling between stages; it is not introduced.

store 现有 cfg(test) 模块机械提取至相邻 `src/store/identity_tests.rs`，保持责任与测试
语义，并为生产代码留出固定单文件预算。该文件是私人开发测试，不加入 Cargo public
allowlist；不放宽 200,000-byte 成员限制或隐私例外，完整验收在开发源码快照运行。
精简 package 的 `cargo test` 仍不支持，缺少冻结 fixture 和该私人测试模块。

The existing store cfg(test) module is mechanically extracted to adjacent
`src/store/identity_tests.rs`, preserving responsibility and test semantics while
leaving room within the fixed per-file production budget. It is a private developer
test, excluded from the public Cargo allowlist. The 200,000-byte member cap and
privacy exceptions are unchanged; full acceptance uses the developer source
snapshot. Tests in the reduced package remain unsupported because frozen fixtures
and the private test module are absent.

验收必须同时检查正例与失败闭合反例：PID reuse/generation 与目标坐标一致；同流收据
及可复核的 raw 字节；raw 改字节、截断、缺失、读取失败；manifest/marker/版本 tuple
错误；未知信号和无 root exit 不制造 target；旧记录不升级；v4 格式保持；v5 的
0/1/2/3 与 text/JSON 一致。A18 的 v5 未实施 outcome-to-all-evidence 映射及样本
省略总数；这些由下文显式 v6 扩展承接，v5 sample 仍不可被宣称为全部证据。

Acceptance must cover both accepted evidence and fail-closed counterparts: PID
reuse/generation and target coordinates, same-stream receipts and verifiable raw,
changed/truncated/missing/unreadable raw, invalid manifest/marker/version tuples,
unknown signals and absent root exits without fabricated targets, no legacy
upgrade, preserved v4 format, and v5 text/JSON parity for 0/1/2/3. DEC-05's
outcome-to-all-evidence mapping and sample omission totals were not implemented
by A18 v5; the explicit v6 extension below adds them. V5 bounded samples must
not be described as all evidence.


### 保存的退出数值合同 / Saved numeric outcome contract

trace-v1 录制明确限于已支持的 Linux x86_64；不支持的 capture 平台在创建记录
或启动目标前拒绝。metadata4 的退出文本、所引用 exit 事件和目标数值必须一致。
纯 model helper 固定 v1 的文本词汇与 signal 编号；capture 的新协议分支使用同一
helper，旧录制继续使用原 decoder，完整性/可靠性归约政策由同一 reducer 共用。
加载不调用主机 libc 的动态 SIGRTMIN，也不运行 raw parser。普通退出值为 0～255；
标准 Linux signal 为 1～31，strace `SIGRT_n` 为 32+n（0～32），glibc 拼写
`SIGRTMIN` 为 34、`SIGRTMIN+n` 为 34+n（0～30），`SIGRTMAX` 为 64。合法未知
signal 名称保留原始引用，但没有已知 target 数值，不能由 collector 补造；未知
不是可信成功。非法退出文本、矛盾数值或用 128+signal 替代 signal 事实失败闭合。
历史 metadata1～3 继续保留其 opaque raw outcome 合同，不追溯强化。

Trace-v1 capture explicitly supports Linux x86_64; another capture platform is
rejected before record creation or target startup. Metadata4's saved exit text,
referenced exit event and numeric target fields must agree. A pure model helper
freezes the v1 vocabulary and signal numbers; the new capture branch uses that
same helper, while legacy capture retains its decoder. Both share the existing
completion/reliability reducer. Loading calls neither host libc's dynamic
SIGRTMIN nor a raw parser. Normal exits are 0–255. Standard Linux signals are
1–31; strace `SIGRT_n` is 32+n (0–32), glibc-spelled `SIGRTMIN` is 34 and
`SIGRTMIN+n` is 34+n (0–30), and `SIGRTMAX` is 64. A valid unknown signal name
retains its references without known numeric target fields; collector facts
cannot fill them or establish trusted success. Malformed exit text, contradictory
numbers and replacing a signal fact with 128+signal fail closed. Historical
metadata1–3 retains its opaque raw-outcome contract without stronger retrospective
interpretation.

## 显式 v6：已保存 outcome 全量引用 / Explicit v6 saved-outcome references

本轮实现已进入固定候选验收，结果另行留证。`diff --report-version 6` 显式输出
schema v6 和 `evidence_contract="outcome_event_references_v1"`；默认旧/旧 v4、
自动 trace-v1 v5 与显式 v5 的字段、样本、排名保持原合同。录制与存储 tuple 不变。
v6 沿用 v5 的采集/结构化/raw/目标来源可信要求：显式旧/旧 v6 或新/旧 v6 的普通
比较为 3，不为旧记录补证。任一工具/加载/损坏错误为 2，`observed_differences`
为 null、`changes=[]`；即使两侧都未加载，显式请求仍保持 schema v6 及合同字段。

This implementation enters fixed-candidate acceptance; its result is recorded
separately. `diff --report-version 6` explicitly emits schema v6 and
`evidence_contract="outcome_event_references_v1"`. Default old-to-old v4,
automatic trace-v1 v5 and explicit v5 keep their fields, samples and ranking.
Recording/storage tuples do not change. V6 inherits v5's capture, structured,
raw and target-provenance trust requirements: ordinary explicit old-to-old or
new-to-old v6 returns 3 without backfilling old evidence. Tool/load/corruption
errors return 2 with null `observed_differences` and `changes=[]`; the explicitly
requested schema and contract remain present even if neither input loads.

每项 change 增加 `evidence_mapping`。file/exec change 的 `observation` 明确保存
kind、operation、path 的比较键；不从 subject 文案猜键。两侧 `outcomes` 按 outcome
排序，每组保存 `event_count` 和全部 `events[{index,evidence}]`。index 是原保存
事件 vector 的零基索引，evidence 保留原 file/一基含尾 start/end 行。组内按原
vector 顺序保存，重复坐标也保留独立 index；没有该键的一侧可以明确为零事件。
全量仅指该变化键的已保存 normalized observations，不包含未变化的键、不承诺
raw 全 syscall 已归一化，也不重新解析证据。次数、顺序、PID 和 generation 不
进入跨 run 身份，重复次数变化本身不产生新的 difference。

Each change adds `evidence_mapping`. File/exec `observation` retains the typed
kind/operation/path comparison key, without guessing it from display text. Each
side's sorted `outcomes` retain an `event_count` and every
`events[{index,evidence}]` reference. Indices are zero-based positions in the
original saved vector; evidence preserves original file and one-based inclusive
line coordinates. References keep vector order and duplicate coordinates with
distinct indices. A side missing that key can assert zero events. Completeness
covers saved normalized observations for that changed key, excludes unchanged
keys, does not promise normalization of every raw syscall and requires no raw
reparse. Counts, order, PID and generation never become cross-run identity;
duplicate-count changes alone do not create a difference.

两侧 `source` 区分四种来源：`normalized_events` 为聚合的保存事件；
`target_provenance` 为 finalized trace-v1 所选目标退出；`metadata` 为命令、CWD、
采集状态/完整性/warning 数及选中环境等 metadata 比较；`withheld` 为 unfinalized
输入刻意扣留的观察。metadata 比较保留 `metadata_fields`，不能伪造 syscall
坐标。metadata/withheld 的 `event_count` 与 `omitted_sample_count` 为 null，表示
不适用/不能断言，而非零事件；`outcomes`、样本 index 为空。目标比较使用保存的
target exit 引用，不以其他 exit 或 collector 补造；缺可用 provenance 时保留
metadata 来源。来源只是取证类型，不会升级既有可靠性。
目标退出的 singleton outcome 等于 metadata 的目标比较值（code/signal/必要时
raw fallback），不等于引用 exit event 的 raw outcome；file/exec 分组使用保存
事件的 outcome，消费者不能把两个分组规则混用。

Side `source` distinguishes `normalized_events` (saved aggregate observations),
`target_provenance` (a finalized trace-v1 selected target exit), `metadata`
(command, CWD, capture state/completeness/warning count and selected environment
facts), and `withheld` (observations deliberately withheld for unfinalized input).
Metadata comparisons retain `metadata_fields` without inventing syscall
coordinates. Metadata/withheld `event_count` and `omitted_sample_count` are null,
meaning inapplicable or unavailable rather than zero events; outcomes and sample
indices are empty. Target changes use the saved selected exit, never another
exit or the collector. Without usable provenance, metadata remains the source.
Source describes attribution and does not strengthen reliability.
The target-exit singleton outcome is the compared metadata target value
(code/signal/optional raw fallback), rather than its exit event's raw outcome.
File/exec groups use saved event outcomes; consumers must keep these rules distinct.

旧平面 evidence 每侧仍最多三条，可用观察的抽样与顺序保持；
`sample_limit_per_side=3`。显式 v6 先执行既有 DTO 校验；通过校验后，合法
unfinalized 侧的 file/exec 事件不进入聚合键、outcome 集合、平面样本或全量映射，
这也包含内部调用者提供的合法非空 metadata-v3 DTO。CLI loader 原本就返回空
事件向量；该规则封闭内部准入边界，不修复或重写输入。其他侧仍可贡献自己的已加载
观察，metadata 比较保留。withheld 的 outcomes/引用为空、事件/省略数为 null，
状态仍为 3；损坏 DTO 先拒绝为 2，unverified 事件仍可展示。unfinalized 侧的
Exit status 平面样本也清空，与 `withheld` 无引用保持一致；v4/v5 原行为不变。顶层
`target_provenance` 可以保留 metadata 中保存的引用，但该侧 provenance quality
为 unverified，不能当成 finalized outcome 映射。
两侧 `sample_event_indices` 精确对应旧样本的原 event index，`sample_count` 为
展示数，`omitted_sample_count=event_count-sample_count` 只描述**旧样本省略**。
全量 `outcomes.events` 不截断，不把样本省略说成完整映射缺失。文本保留前五和
后续折叠，额外展示来源、全量数、样本数与样本省略数；folded change 及完整引用
仍在 JSON。report 只呈现 diff 已计算的字段，不加载数据、重算数量或改变退出码。

Flat compatibility evidence remains capped at three per side; usable
observations keep their sampling/order and `sample_limit_per_side=3`. Explicit
v6 first applies the existing DTO contracts. After validation, a valid unfinalized
side's file/exec events contribute no aggregate keys, outcomes, flat samples or
full mappings, including legal nonempty metadata-v3 DTOs from internal callers.
The CLI loader already supplies empty event vectors; this closes the internal
admission boundary without repairing or rewriting inputs. Other sides may still
contribute their loaded observations, and metadata comparisons remain. Withheld
outcomes/references are empty and event/omission counts are null, with status 3;
corrupt DTOs are rejected first with 2, while unverified events remain visible.
Unfinalized Exit status flat samples are also cleared to match the reference-free
`withheld` mapping; v4/v5 retain their original behavior.
Top-level `target_provenance` may retain metadata's saved references, but that
side's provenance quality is unverified and does not authorize a finalized
outcome mapping. Side `sample_event_indices` correspond
exactly to those samples' original event indices; `sample_count` is the displayed
count. `omitted_sample_count=event_count-sample_count` describes **omission from
the small sample only**. Complete `outcomes.events` vectors are never truncated.
Text retains its first-five and later-folding policy and adds source, total,
sample and omitted-sample counts; folded changes and full references remain in
JSON. Report renders diff's computed fields without loading data, recounting or
changing the status.

实现边界仍为 diff → model、report → diff/model；没有新 I/O、生产依赖、trait 或
global state。最简单替代是只增大三条样本上限，但仍不能显示全部 outcome 来源
或判断省略，因此采用显式版本化映射。v6 为变化事件持有线性规模的 owned 引用；
现有 pretty JSON renderer 还生成完整输出 String，因此内存与输出量随已保存
变化事件增长。已有存储数量/字节预算不是任意 RLIMIT/OOM 恢复保证，本轮不引入
流式输出或输出配额，也不宣称跨环境完全 hermetic、断电持久性或原始来源认证。

Dependencies remain diff → model and report → diff/model, with no new I/O,
production dependency, trait or global state. Simply raising the three-sample
limit is the smallest alternative, but still cannot expose every outcome source
or quantify omissions; the explicit versioned mapping supplies that contract.
V6 owns references linear in changed saved events. The existing pretty JSON
renderer also allocates the complete output String, so memory/output grow with
changed observations. Storage count/byte budgets do not guarantee recovery under
arbitrary RLIMIT/OOM conditions. This work adds neither streaming output nor
output quotas and makes no hermetic, power-loss or source-authentication claim.

验收须覆盖多于三种 outcome、重复事件/同坐标不同 index、双端 unfinished/resumed
坐标、单侧键、PID generation、target 及 metadata 来源、unfinalized withholding；
既有样本/排名/0/1/2/3 与 text/JSON 同步；旧格式字段与冻结五例不变；无效 DTO/raw
损坏时无普通结论和部分映射。每项正反例、固定 tree、工具/二进制/命令与原始
日志另存本轮账，不回写历史 receipts。

Acceptance covers more than three outcomes, duplicate occurrences/coordinates
with distinct indices, unfinished/resumed ranges, missing-side keys, PID
generations, target/metadata sources and unfinalized withholding. Samples,
ranking, four-way status and text/JSON stay aligned; earlier fields and the frozen
five cases remain unchanged. Invalid DTOs or damaged raw expose no ordinary
conclusion or partial change mapping. Paired controls, fixed tree, tools/binary,
commands and original logs receive this run's ledger without rewriting history.

## 2026-10-07 capture completion 私有边界

目标选择与 generation 完成分析只消费同一个 Parsed 的时间线；provenance 复制
原事件索引/坐标，不借 collector 结果、不重解析历史记录。提取私有子模块以限制
职责和依赖；最小替代是继续保留单文件，但无法降低当前 capture 的职责集中。
窄只读方法替代父模块直接访问字段；不引入 trait、生产依赖、I/O 或新全局状态。
provenance 的 Parsed 必须是构造 Completion 时的同一份输入，保留既有前提。

Target selection and generation completion consume the same Parsed timeline;
provenance copies original indices/coordinates without borrowing collector facts
or reparsing records. A private child module limits responsibility and dependencies.
Keeping the single file is the smallest alternative but retains its concentration.
Narrow read-only methods replace direct parent field access, without traits,
production dependencies, I/O or new global state. Provenance retains the existing
precondition that its Parsed input is the one used to construct Completion.

本批只迁移 completion 及相应测试；混合归约断言在编排边界保留。结果归约、平台
退出解码与 Linux 资源权限的后续拆分分别验收；SignalScope 贯穿事实冻结、提交
和恢复的前提，以及已提交后恢复失败的独立错误，不随文件组织调整而弱化。

This batch moves only completion and its tests; mixed reduction assertions stay
at the orchestration boundary. Later fact/decoder and Linux ownership extractions
receive separate acceptance. File organization must preserve SignalScope through
fact freezing, commit and restoration, including a distinct restoration error
after an already committed record.

## 2026-10-07 退出译码与纯归约

退出原文/平台 wait status 的解释是适配职责；依据已知事实决定 capture 状态是
纯规则。将二者分别放入 `platform_exit` 和 `reduction`，依赖方向为前者 → 后者 →
model，父 record 调用二者。最小替代是保留 decoder 函数参数，但会把 volatile
host ABI 选择藏在归约入口里；本次改为在原冻结边界构造类型化观测，再统一归约。

Interpreting raw exits/platform wait status is an adapter responsibility; deciding
capture state from known facts is pure policy. `platform_exit` depends on
`reduction`, which depends on model, with the record workflow calling both.
Keeping a decoder callback is the smallest alternative but hides host ABI choice
inside reduction; the existing frozen boundary now constructs typed observations
before one shared reducer.

以一个可选 raw/typed pair 防止未知观测与缺失观测分离错误；不采用两份独立 Option。
保留旧原文 warning 规则，它不重新解释 signal 数字。结果只有只读状态/完整性/CLI
和消费式 metadata 投影，测试专用 snapshot 完整对照原事实而不开放生产字段。
没有新生产依赖、I/O、trait、全局状态或采集信号生命周期变更。完整 JSON 与引用
存储只记录内存基线，仍不在本次改造范围。

One optional raw/typed pair prevents unknown observations from becoming missing
observations; two independent Options are avoided. The prior raw-text warning
rule remains and does not reinterpret signal numbers. Results expose read-only
state/completeness/CLI and consuming metadata projection; test-only snapshots
compare every old fact without exposing production fields. There are no new
production dependencies, I/O, traits, global state or signal-lifetime changes.
JSON and reference storage remain outside this refactor; only memory baseline is
recorded.

## 2026-10-07 Linux 资源权限与录制结果边界

将易变 Linux 适配集中在 supervision，稳定共享限额置于 policy；父 record 管理
应用流程与 store/parse 边界，避免监督通过父内部类型/实现建立反向依赖。最小
替代是只改文件位置而保留 child/fd 字段暴露，但它无法限制清理权限；因此父只
持有 opaque scope，并以阶段入口取得 collector/limit/error facts。

Volatile Linux adaptation lives in supervision, with shared limits in policy.
The record workflow owns application/storage/parser boundaries, avoiding a
reverse dependency on parent implementation. Moving files while exposing child/fd
fields is the smallest alternative but does not limit cleanup authority; the
parent instead holds an opaque scope and consumes phase results as facts.

监督需要读取同一 anchored trace 的长度，但不需要认识 store::RunWriter。函数
端口保留真实 anchored adapter 与既有 fixed/failed 测试替代，是当前最小接缝；
不引入 trait 框架或新 I/O。计数闭包检验禁用零调用、相等非超限、首次超限后
停止周期读取与失败不冒充超限。最终长度检查仍由父持有 writer 的阶段执行。

Supervision needs the anchored trace length, not store::RunWriter. A function
port preserves the real adapter and existing fixed/failed alternatives as the
smallest seam, without a trait framework or new I/O. Counting closures verify
zero disabled calls, equality without excess, stopping periodic reads after the
first violation and failure without an invented limit. The parent-owned writer
still performs the final length check.

保持已有底层信号 syscall/errno 语义，不顺带改 pthread API。scope 贯穿事实冻结、
提交和恢复；恢复错误属于 recorder 工具边界，不能改已保存状态或掩盖已提交 ID。
新增 post-commit probe 是开发验收资产，仅进入私有闭包；新增 Rust 源文件逐项
进入公开包精确白名单。没有放宽 privacy 例外、预算或冻结五例，也没有发布。

Existing signal syscall/errno semantics remain; no pthread API change is folded
into this extraction. Scope spans fact freezing, commit and restoration, whose
error belongs to the recorder tool boundary and cannot rewrite saved state or
hide a committed ID. The post-commit probe is a private development acceptance
asset; new Rust sources enter the precise package allowlist individually. Privacy
exceptions, budgets and the frozen five are preserved; no publication occurs.

## 2026-10-07 内存成本治理：diff JSON 呈现

基线为 `f8ef9eaadd421d55ae85e92ee3e21a7f5212bdc7`。先用独立诊断 release
进程测量完整加载、含验证/聚合的 v6 比较与 JSON 序列化，并另测真实 CLI。
每侧 1,000 / 10,000 / 60,000 事件，四种变化分布，各串行三次。分配器只计 Rust
requested Layout；绝对 RSS 包含前置准备，不能将两条命令的峰值相减来归因。
输入是明确标记的合成合同一致性材料，不证明真实采集来源。

Independent diagnostic release processes measure full loading, validated v6
comparison/aggregation/mapping and JSON serialization, alongside the real CLI.
Each side has 1,000, 10,000 or 60,000 events, four change distributions and three
serial repetitions. Allocator counts cover requested Rust Layout bytes; absolute
RSS includes preparation. Subtracting process peaks is not attribution. Synthetic
contract-consistency fixtures do not authenticate real capture.

60,000 事件的大量变化用例产生 131,624,147 字节 JSON，完整 String 容量为
134,217,728 字节。依据这一成本，仅将 diff JSON 改为借用既有 compatibility
view 写入调用方的 byte sink；完整 Report/v6 引用仍由 diff 持有。main 用固定
8 KiB BufWriter 管理 stdout、唯一末尾 LF、显式 flush 与错误。最小替代是缩容
或预分配 String，但前者晚于增长峰值，后者仍持有完整文档。没有改 list/show、
引用存储、加载预算、capture、排名、字段或四态合同，也没有新增输出配额。

The large many-change input produces 131,624,147 JSON bytes and a
134,217,728-byte String capacity. Only diff JSON now writes its existing borrowed
compatibility view to a caller-owned byte sink; diff retains all Report/v6
references. Main owns stdout, a fixed 8 KiB BufWriter, one final LF, explicit
flush and error policy. Shrinking happens after growth; preallocation still
retains the document. List/show, reference ownership, loading budgets, capture,
ordering, fields and four-way status are unchanged. No output quota is added.

序列化、换行或 flush 时的 BrokenPipe 均保留计算结果；其他写入/flush 错误及
非 I/O 序列化错误由 main 返回工具错误 2。失败可留下输出前缀，不保证完整 JSON
已交付。所有路径用 into_parts 拆解缓冲，避免 Drop 在错误分类后重试。report
不打开文件、不加载、不决定可靠性或重算退出码；除调用方提供的 Write 接缝外，
无新依赖、trait 框架或 global state。诊断代码未进入 CLI、公开 API 或包白名单。

BrokenPipe during serialization, LF or flush preserves the computed status.
Other writes/flushes and non-I/O serialization failures become tool error 2 at
main. A failed stream may leave a prefix, rather than a complete delivered JSON
document. Every return uses into_parts to prevent Drop retry after classification.
Report opens no files, loads no records and decides neither reliability nor
status. The caller-provided Write seam adds no dependency, trait framework or
global state. Diagnostics do not enter the CLI, public API or package allowlist.

同条件前后测量、完整门禁、来源账和历史凭据前后核对另存
`target/memory-cost-governance/20261007`，不覆盖旧账，本节不预写验收。完整事件、
v6 引用及比较中间容器仍有线性成本。不承诺任意 OOM/RLIMIT 恢复、跨环境收益、
完全 hermetic 或位级重建，也不以通过验收证明全部源码合理。

Matched measurements, complete gates, line ancestry and historical evidence
checks receive a new private ledger without overwriting old receipts or declaring
acceptance early. Full events, v6 references and comparison temporaries retain
linear cost. There is no arbitrary OOM/RLIMIT recovery, cross-host improvement,
hermetic/bit-identical rebuild or all-source correctness claim.

## 2026-10-07 diff 聚合成本：借用有序合并

基线为已验收树 `fe70b93b822a57948363c1510c40fad6a7e7e32d`。diff 仍仅依赖
稳定 model 合同，以纯函数验证并比较已加载 Run；不加入 I/O、依赖、全局状态或
公开 API。外层聚合树借用 kind/operation/path，内层借用 outcome，并拥有完整
保存事件索引。v6 映射拥有索引与原始 Evidence 的副本，不省略重复观察。两侧
聚合保持到原函数返回，不能把本轮说成提前释放索引或取消完整引用存储。

The accepted tree is the baseline. Diff still validates and compares loaded Runs
through pure functions consuming stable model contracts, without new I/O,
dependencies, global state or public APIs. Outer keys and outcome keys borrow
event strings; buckets own complete saved-event indices. V6 mappings own indices
and original Evidence copies, including repetitions. Both aggregate maps retain
their original lifetime; this change neither releases them earlier nor removes
full reference storage.

私有诊断按实际分配操作计 requested Rust Layout，并冻结各阶段存活量；每侧
60,000 事件的唯一 key 用例中，key 并集构造请求 2,531,168 字节，阶段增量峰值
1,919,968 字节，构造后存活 611,200 字节。原 owned 迭代器在循环中逐步释放
并集；内层 outcome 树请求 54,719,088 字节，是更大的成本。重复用例的并集
请求仅 1,040 字节。这些是指定输入和诊断构建的计数，不是两个进程 RSS 相减。

Private diagnostics count requested Rust Layout at actual operations and retain
liveness checkpoints. At 60,000 events per side with unique keys, union
construction requests 2,531,168 bytes, peaks at 1,919,968 incremental live bytes
and retains 611,200 bytes after construction. Its owned iterator releases nodes
progressively. Inner outcome trees request a larger 54,719,088 bytes; the repeated
key case requests only 1,040 union bytes. These input-specific diagnostic counts
are not differences between process RSS peaks.

最小替代是保留两棵已排序聚合树，以借用迭代器直接合并：较小 tuple 推进一侧，
相等推进两侧，耗尽后继续另一侧尾部。每个 key 恰好产出一次，顺序仍为
kind/operation/path 的既有 Ord；因此比较生产顺序和后续排名平局顺序保持。
只取消额外并集树和逐 key 的两次查找，辅助迭代状态不随 key 数增长。保留现状
也满足合同，但继续付出可避免的容器成本；改用 singleton outcome 表示或消费
聚合以提前释放索引会扩大表示/生命周期改动，本轮不采用。

The smallest alternative merges borrowed iterators over the already ordered
maps. Advance the lesser tuple, advance both on equality, and drain either tail.
Each key appears once in the established kind/operation/path Ord, preserving
comparison-production order and later ranking ties. This removes the extra
union tree and two lookups per key with constant auxiliary iterator state.
Keeping the union is correct but retains avoidable cost. Singleton outcome
representations or consuming aggregates would expand representation or lifetime
changes and are deferred.

先在旧算法上冻结交错 key、空侧/尾部、重复 outcome/坐标、合法 withholding 和
损坏先校验的回归及 v4/v5/v6 字节，再改遍历。样本三阶段选择、完整索引、证据
坐标、普通 unverified 观察、unfinalized 验证后扣留、字段和四态合同不变。
同条件测量和新候选验收另存 `target/diff-aggregation-governance/20261007`；
这里记录设计依据，不预写验收结论，不改已验收流式 JSON 或 capture。

Old-algorithm regressions and v4/v5/v6 bytes freeze interleaved keys, empty sides,
tails, repeated outcomes/coordinates, valid withholding and validation before
corrupt-input withholding. Three-pass sampling, full indices, coordinates,
ordinary unverified observations, fields and four-way status remain. Matched
measurements and candidate acceptance receive a separate private ledger. This
records design grounds, not acceptance in advance; accepted streamed JSON and
capture remain unchanged.

## 2026-10-09 diff 内层 outcome：单项内联、多项有序树

基线为已验收树 `c73bccb76f1de9215651083ac49b67cebf3f7f94`。本轮仅改变
`diff` 私有 outcome 桶表示；稳定 model 校验、外层借用有序合并、样本选择、完整
映射、可靠性与退出码算法不变。样本筛选的两个 lookup 调用仅移除自动解引用所
替代的显式星号，以遵守严格 Clippy。无新生产文件、依赖、公开 API、I/O 或
trait 框架。完整事件和索引属于证据合同，不能靠去重或裁剪取得收益。

The accepted baseline is `c73bccb76f1de9215651083ac49b67cebf3f7f94`. Only the
private outcome bucket representation changes. Model validation, outer ordered
merging, sampling, full mapping, reliability and exit computation preserve their
algorithms. Two sampling lookup calls omit redundant explicit dereference for
strict Clippy. There is no new production file, dependency, public API, I/O
or trait framework. Full events and indices remain evidence requirements.

封存的原十二组输入均为单 outcome；这不是对真实运行分布的普遍假设。独立新增
十二组混合、多 outcome 及逆序高基数输入，不改旧输入或冻结五例。构造实验将两侧
实际外层树一起保留，分别记录 inner、outer、index 的 requested Layout、端点存活
和峰值，并核对完整有序投影、索引容量与释放净账。累计请求不等于 RSS；完整比较
的标签只指分配请求位置和执行释放位置，没有历史指针所有权追踪，峰值不能相加。

The twelve sealed original inputs have singleton outcomes, not a general claim
about real captures. Twelve separate mixed, multi-outcome and reverse-order
fixtures add pressure without changing old inputs or the frozen five cases.
Controlled construction retains both real outer maps and records requested
Layouts, endpoint retention and peaks, checking full ordered projections, index
capacities and destruction balances. Cumulative requests are not RSS. Full diff
tags classify request and release execution sites without tracking historical
pointer owners; peaks are not additive.

比较原树、排序向量、单项内联/多项树三个具体方案后，采用最后一个：Empty 用作
默认及缺侧视图，Single 借用 outcome 并拥有原索引 Vec，第二个不同 outcome 将原
Vec 移入 Multiple 树；同 outcome 重复只追加索引。没有移除、降级或丢弃引用的 API。
完成聚合时每个非空桶及向量非空；插入方法返回到既有 push 之前允许短暂空向量。
私有迭代器对单项返回一项，对多项委托树，保留字典序、借用和准确长度提示。

Of the original tree, sorted vector and inline-single/tree-multiple alternatives,
the last is selected. Empty supplies default and missing-side views. Single
borrows the spelling and owns its full index Vec. A second distinct outcome
moves that Vec into Multiple; repeated identical outcomes only append indices.
There is no removal, downgrade or reference-discard API. Completed aggregate
buckets and vectors are nonempty; insertion may briefly create an empty Vec
before the existing push. A private iterator returns the singleton or delegates
to the lexical tree, preserving borrowed traversal and exact length hints.

收益必须扣除外层增量：当前工具链受控测量中桶由 24 增至 40 字节，外层存值由
72 增至 88 字节。每侧 60,000 事件的单 outcome 场景，内层累计/保留节点成本约
54.72 MB 消失，外层增加约 3.52 MB，构造端点净少约 51.20 MB，完整索引容量不变。
多 outcome 的 60,000/multi12、multi64 则分别净增 96,944、17,392 字节。排序向量
虽然较小，但逆序 60,000 outcome 的受控构造中位耗时约为原树的 27 倍，故不采用；
这些含原子诊断计数的局部时间不是产品速度结论。

Benefit includes the outer-node cost: this toolchain measures bucket size 24→40 B
and outer stored pair 72→88 B. With 60,000 singleton-key events per side, inner
nodes account for about 54.72 MB, outer retention increases about 3.52 MB, and
controlled retained construction net falls about 51.20 MB; index capacities do
not change. The 60,000/multi12 and multi64 cases instead increase by 96,944 and
17,392 B. Sorted vectors are smaller but the 60,000-outcome reverse-order probe
takes about 27 times the original tree median, so that option is rejected.
Atomic-instrumented local times do not establish product speed.

2026-10-10 勘误：本节先前的 24→48 / 72→96 字节说明与绑定 metrics 不符。
原 `btree` 的两侧桶/外层存值为 24/72 字节，`singleton_btree` 及最终 attribution
候选为 40/88 字节。这里只纠正当前说明；旧封存报告和原测量不覆盖。新勘误
生成器读取、核验绑定指标并检查所有选定行及两侧一致，不从说明文字推导尺寸。

2026-10-10 erratum: the former 24→48 / 72→96 B explanation contradicted the
bound metrics. Both sides of `btree` report 24/72 B; `singleton_btree` and the
final attribution candidate report 40/88 B. This corrects the current explanation;
sealed reports and original measurements remain intact. The new erratum generator
reads and verifies bound metrics, checking every selected row and both sides,
rather than deriving sizes from prose.

Run 仍拥有事件字符串及原 Evidence；聚合借用字符串并拥有完整索引和容器，直到
原比较返回边界释放。Report 为消费 Run 之后的呈现独立拥有字符串、坐标和完整
引用；两侧聚合与映射仍同时存活。本轮不提前释放、不改比较身份，也不改已验收的
流式 JSON、capture 或存储。全部规模的同条件整体数据、错误变体和固定树验收另存
`target/diff-outcome-governance/20261009`，本节不预写候选验收或跨环境收益。

Runs still own event strings and original Evidence. Aggregates borrow strings
and retain full indices and containers until the same comparison return boundary.
Reports independently own strings, coordinates and full references for display
after Runs are consumed. Both aggregate maps still coexist with mappings. This
cycle changes neither lifetimes nor comparison identity, accepted streaming,
capture or storage. All-scale matched results, executed faulty variants and
fixed-tree acceptance receive a separate private ledger; acceptance and
cross-environment benefit are not predeclared here.
