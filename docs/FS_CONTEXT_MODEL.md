# FsContext：共享文件系统上下文模型

日期：2026-09-20；trace-v1 持久化边界补充于 2026-10-07。本文描述记录时的解析器模型及其显式保存合同，历史记录仍按原版本读取，不改变其结论。

## 目的和边界

Linux 的 `CLONE_FS` 让两个任务共享一个 `fs_struct`，其中包括 CWD。旧模型一旦看见共享 FS，就把整次运行的 CWD fallback 全部清空。这既不能传播一个已证实的共享 `chdir`，也会影响无关的私有分支。

当前模型只传播能由成功 syscall 和已解析 clone flags 证明的关系。它继续把历史 CWD 与当前物理目录身份分开：相对 `access` 等没有当次 dirfd/返回 fd 路径注释的事件仍保留 `relative:... [cwd unresolved]`，即使 `cwd_before` 有值。它不会用运行结束后的文件系统、inode 或字符串拼接猜测目录重命名后的名字。

`cwd_before` 是最近观察到的或词法描述的上下文，不是“该物理目录当时可验证的绝对路径”。一次 syscall 的完整 `-yy` dirfd/返回 fd 注释、或原始绝对参数，优先作为该事件的路径证据。

## 运行期状态机

状态仅存在于解析期的 `TraceParser` 状态机中，由生产路径逐行调用
`push_line` 并以 `finish` 完成归约；`parse_with_cwd` 只是当前保留的兼容与测试适配器，
不会形成第二套状态机：

```text
TaskGeneration(pid#ordinal) ── fs ──> FsContext(id, cwd observation, uncertain, member count)

initial observed task              -> private FsContext(initial CWD)
clone/clone3 + CLONE_FS success    -> child aliases parent's FsContext
fork/vfork or clone without FS     -> child gets a private copy of observation
clone/clone3 + namespace success   -> child gets an uncertain private context
unshare(CLONE_FS) success          -> caller gets a private copy
unshare(namespace with proven FS split) success -> caller gets an uncertain private context
unshare(namespace without proven FS split) success -> existing connected context becomes uncertain
exec success/failure               -> task keeps its FsContext
successful chdir/fchdir with absolute per-syscall evidence -> updates that FsContext only
successful chdir/fchdir without it -> connected FsContext becomes uncertain
successful mount/umount2/move_mount/mount_setattr -> all active contexts become uncertain
task exit                          -> decrements membership; last member removes context
```

所有已识别的 namespace flag 都是保守边界：`CLONE_NEWTIME`、`CLONE_NEWNS`、`CLONE_NEWCGROUP`、`CLONE_NEWUTS`、`CLONE_NEWIPC`、`CLONE_NEWUSER`、`CLONE_NEWPID`、`CLONE_NEWNET`，以及相应 syscall 接受的 EMPTY_MNTNS 位。成功 clone/clone3 只让新 child 的私有 context 不确定，parent 和无关分支保持原 certainty；当前 parser 尚不支持 `CLONE_FS` 与 namespace flag 同时出现，整体按 `Unknown` fail closed，这不是声称 Linux 禁止全部此类组合。成功 unshare 只有明确的 `CLONE_FS`、`CLONE_NEWUSER`、`CLONE_NEWNS` 或 `UNSHARE_EMPTY_MNTNS` 才证明 FS 分离；这时 caller 的新 context 不确定，同一旧共享组保持旧 context，即使 entry/return 跨行。仅 NEWTIME、NEWCGROUP、NEWUTS、NEWIPC、NEWPID 或 NEWNET 时不能猜测分离：保留现有 aliases，令 connected context 不确定，隔离的 private 分支保持原 certainty。失败调用不转换；未知 symbolic/numeric flags 仍走 `Unknown`，不推断 sharing 或 splitting。

The unshare distinction is grounded in [Linux v6.12 `kernel/fork.c`, commit `adc218676eef25575469234709c2d87185ca223a`, lines 3029–3044 and 3083–3099](https://github.com/torvalds/linux/blob/adc218676eef25575469234709c2d87185ca223a/kernel/fork.c#L3029) and [Linux v7.2 `kernel/fork.c`, lines 2987–3002 and 3041–3059](https://github.com/torvalds/linux/blob/v7.2/kernel/fork.c#L2987). The latter includes EMPTY_MNTNS implying NEWNS, which implies FS. These sources support the enumerated flag model; they do not verify every vendor kernel, unknown flag, or namespace layout. Namespace creation without a proven FS split retains known aliases and invalidates their context conservatively.

解析器从 normalized event 的原始 evidence 顺序建立 transient `TaskTimeline`，排序键固定为 `(line, end_line, original_index)`。每次成功 spawn 都创建新 generation。唯一的 `Initial` 和由成功 spawn 引入且没有 live PID collision 的 `Spawned` generation 可作为可信身份；`Unintroduced`、退出后没有新 spawn 的 `Reappeared`、以及覆盖仍存活 PID 的 `LiveCollision` 都产生 `task.identity:` warning，并把相应事件和 context 标成不确定。这样旧 generation 的 FS 状态不会按裸 PID 借给新 generation。

`FsState` 使用 `TaskGenerationId` 到 context 的 `BTreeMap`，另有从 context ID 到状态的 `BTreeMap`。Context membership 是计数，不复制完整成员集合，因此 transient state 为 O(generations + contexts)，且输出不依赖 hash iteration order。generation、origin 和 timeline 都不进入默认 Event v2。显式 `record --evidence-contract trace-v1` 将本次 timeline 的 actor/child PID 内一基 generation ordinal 保存为 Event v3；origin/context 内部状态不导出，warning 和不确定性不清除。读取时只验证保存标注与固定合同的一致性，不重跑 raw parser，也不改写历史结论。

Capture completion 直接消费这条 transient timeline，不再按裸 PID 重建身份。Root 是 evidence 顺序中首个成功 exec 所属的 generation，只有该 generation 自己的 exit 才能确定 target exit；早于 exec 的 exit、同 PID 旧 generation 的 exit，以及后续 generation 的 exit 都不能替代它。Missing completion 也逐 generation 计算：只有一个 generation 的 PID 保持原有裸 PID 文案，PID 被复用时用 `PID#ordinal` 区分，且警告按该显示标签稳定排序。默认格式不增加持久化字段。显式 trace-v1 的 metadata v4 将所选 root PID/ordinal、exec/exit 零基事件索引与原始一基 evidence 坐标一起保存；缺失同代 exit 时不补造 provenance，不借 collector 结果。所有原始 evidence coordinates 保持不变。

Unfinished prefixes also respect observed lifetime boundaries. A recognized task exit discards its pending prefix with a warning; a subsequent resumed row cannot borrow it across PID reuse. At successful child introduction, only a prefix predating the spawn's entry evidence is discarded and warned. A child prefix starting after that entry may legitimately precede the parent's resumed return and remains eligible for its own resumed row, preserving both raw bounds. These rules reject corrupt synthetic lifetime crossings without inventing normalized events. For a live PID collision with claimed `CLONE_FS`, both the displaced context and the source connected context become uncertain; a proved private collision does not weaken an independent source branch. 该处理只约束新 trace 的证据归约，不按裸 PID 借用旧 generation，不改写已保存记录或原始行号。

### Return-value evidence boundary / 返回值证据边界

Successful filesystem actions and child creation are derived from the numeric syscall return, never from diagnostic display text. A `-1` result requires an uppercase `E...` errno/restart token; an unsupported spelling emits a raw-line warning and makes a newly captured run incomplete. Synthetic malformed input such as `chdir("/changed") = -1 success` or `unshare(CLONE_FS) = -1 success` cannot update or split a context. Legal numeric successes and errno failures keep their existing outcomes and evidence coordinates.

文件系统转换和 child 创建只由数字返回值证明；失败后的显示文本不能成为保留的 `success` outcome。畸形失败行保留在 raw evidence 中并产生 warning，使新记录为 incomplete。成功的 CWD 转换使用 parser 内部 action，后续归约不再用显示 outcome 决定是否切换目录。该边界只影响新记录的解析，不重新解释或改写历史记录。

### Flags ABI 合同

`clone`、`clone3` 和 `unshare` 使用三套独立 decoder；相同位值不会跨 syscall 复用语义：

- legacy `clone` 的低 8 位是 `CSIGNAL`。例如 `0x211` 是 `CLONE_FS|SIGCHLD`；低 8 位若不是 0～64 的有效信号值则无法作为可信关系。
- `clone3` 从独立的 `exit_signal` 字段读取退出信号，其 flags 中的 `0x80` 是 `CLONE_NEWTIME`，不是 `CSIGNAL`。
- `unshare` 使用自己的 allowed mask；`0x00100000` 在这里是 `UNSHARE_EMPTY_MNTNS`，不是 clone 的 `CLONE_PARENT_SETTID`。

每套 decoder 都先验证完整表达式。任何未知 symbolic term、allowed mask 外的 numeric bit、该 syscall 不接受的 flag、已验证的 Linux ABI 冲突或尚未支持的 FS-plus-namespace clone 组合都会产生 `Unknown` effect。`CLONE_FS` 不能让同一表达式中的未知项被忽略。Capability、资源限制等运行时条件不被误当 flags 冲突；保守拒绝的组合也不等于 OS 绝对禁止。`Unknown` 不会被推断为 shared 或 private，而是让相关上下文 fail closed 并产生 warning。已识别 namespace unshare effects 按是否证明 split 区分，供上述局部状态转换使用。

### Capture/parser syscall 合同

`src/parse` 拥有唯一且有序的 syscall 合同，将每个采集名称映射到 parser/state-transition 操作。Capture 从该表生成 `strace -e trace=...` selector，不维护第二份过滤列表。在打开 record writer 或启动目标前，capture 会要求选定的 strace 解析完整 selector；任一必要 syscall 不受支持时直接失败，不能静默采集一份随后被 parser 误当作完整的证据。

合同包含 `mount`、`umount2`、`move_mount` 和 `mount_setattr`。它们成功返回只证明挂载拓扑可能改变；当前 transient model 尚不跟踪 mount-namespace membership 或 mount-tree identity。因此成功调用会把全部 active FS context 标为不确定（`AllActiveContexts`），包括原本私有的分支；失败调用不执行转换。normalized operation、outcome、raw file 与 evidence 起止行保持不变，不新增 event 或 metadata 字段。

## Evidence ordering and uncertainty

`strace` can print a syscall entry and its resumed return on different lines while another task runs in between. A pending CWD change or ordinary FS-sharing transition is held until its return evidence. A successful private fork/clone whose creation instant is not ordered keeps a one-way `PendingForkSnapshot` only on its source context; the upper bound is `min(spawn.end_line, child generation first actor event.line)`. The source holds a `Vec`, so concurrent private forks do not overwrite one another. The child never carries this pending marker: child activity before the parent's resumed return is accepted and itself closes the snapshot window.

An overlapping successful CWD, unshare, root, or namespace transition invalidates the source context, the corresponding spawn evidence, and that snapshot's child context. An expired window, another ordinary fork, or a failed transition does not. A namespace clone/unshare branch that is already uncertain still absorbs its own snapshot ambiguity without erasing an independently known source-group CWD transition. Earlier invalidated pending events are marked uncertain and metadata receives an `fs.context:` warning at the conflicting syscall's raw evidence line.

The following conditions are intentionally incomplete rather than guessed:

| Evidence condition | Result |
|---|---|
| clone/clone3 flags missing or cannot establish FS sharing | relevant parent/child context is uncertain |
| successful relative `chdir`, or `fchdir` without a decodable absolute annotation | connected context is uncertain; the relative operand is not joined to historical CWD |
| overlapping CWD transitions or a transition inside a private-fork snapshot window | connected source, spawn evidence, and affected child context are uncertain |
| successful `setns`, `chroot`, or recognized clone/unshare namespace change | only the transition's modeled connected/child/caller scope is uncertain; namespace-only unshare retains aliases unless flags prove an FS split |
| successful `pivot_root` while namespace membership is not modeled | all active contexts are uncertain |
| successful `mount`, `umount2`, `move_mount`, or `mount_setattr` while mount-namespace membership is not modeled | all active contexts are uncertain |
| unintroduced, reappeared, or live-colliding task identity | generation receives an identity warning and uncertain context |
| a relative operand lacks per-syscall path evidence | path remains relative and the event is uncertain |

只有完整通过对应 syscall decoder 的 flags 表达式才能证明 sharing、copy 或 split 关系；一个已知位不能掩盖同一表达式中的未知或冲突证据。

## Exec 与生命周期

Linux 的 `copy_fs`/`unshare_fs` 行为与本地 probe 表明，普通 `execve` 本身不会拆分共享的 `fs_struct`。因此成功或失败的 exec 尝试都保留原 context。Probe 验证：共享 child 在 `execve` 后仍会观察到 parent 随后的 CWD 变化，而私有 fork child 不会。

模型尚不能跨越 non-leader-thread exec 的特殊输出迁移 task identity；在该输出中，strace 可能报告 PID 改变或某 task 被 exec 取代。此类未支持的身份外观通过不可信 generation origin 分类，在专用 transition 被建模并测试前保守保持 incomplete。Detached process、未追踪的外部目录重命名、未观察到的 namespace transition、directory-fd reuse 与 trace truncation 也遵循同一失败闭合原则。

## Persistence and compatibility

Default recording retains events v2, metadata v3 and finalization marker v1. Existing v1/v2/v3 records remain readable under their original storage policy.

Explicit `record --evidence-contract trace-v1` writes events v3, metadata v4, raw manifest v1 and marker v2. Actor and child generations are one-based ordinals per PID within this trace. Readers check saved annotations in `(line, end_line, insertion_index)` order: an actor retains its active ordinal until exit; a reappearance allocates the next ordinal; every successful spawn allocates the next child ordinal, including a live collision. Closed or replaced generations cannot be borrowed back. This checks saved-contract consistency, not physical process identity or the correctness of every normalization inference.

Metadata v4 retains the capture-time root PID/ordinal, zero-based exec/exit vector references and original one-based evidence coordinates. The first successful exec in evidence order selects the root, and only its same-generation exit can supply target provenance. No root exit means no fabricated provenance. Raw integrity is independently classified from a same-normalization-stream receipt; it does not prove origin, causation or power-loss durability. See [the accepted version contract](REFACTOR_DECISIONS.md).

Clone flags are consumed from raw trace while creating a new record and are not retrofitted into old normalized events. A historical record is not re-parsed or upgraded when read. v1 records have unknown capture confidence and unverified storage; v2/v3 preserve their recorded warnings/completeness. Thus a record without enough historical sharing evidence cannot gain a new `complete` claim merely because a newer binary reads it.

## Validation scope

The regression suite covers generation reuse, unintroduced tasks, live collisions, fresh respawn, child-before-resume ordering, pthread and cross-thread-group `CLONE_FS`, private fork/clone inheritance, bounded private-fork snapshots (overlap, child-before-resume, concurrent forks, expired windows, and failed transitions), mixed shared/private branches, absolute and relative success/failure of `chdir` and `fchdir`, `unshare(CLONE_FS)`, exec persistence, multiple member exits, unfinished/resumed ordering, the three syscall-specific accepted/fail-closed flags matrices, namespace scope, root-change invalidation, the four mount-topology syscalls, selector/parser parity and preflight failure before target execution, and the prior CWD-rename safety rule. Each mount-topology syscall has a successful all-context invalidation case paired with a failed no-transition counterexample. A separate ignored Linux acceptance probe runs a real mount and unmount in a user-owned mount namespace. The development full gate performs the same user+mount+tmpfs capability preflight and records an explicit `skipped` result when the host cannot support it; the acceptance itself never converts that condition into a passing result. The flags matrix includes legacy `CSIGNAL`, clone3's separate `exit_signal` and `CLONE_NEWTIME`, unshare's `UNSHARE_EMPTY_MNTNS`, every recognized namespace class, unknown symbolic/numeric bits, operation-invalid bits, and contradictory combinations. The Linux integration probe uses pipe synchronization and validates actual payload content as well as parsed path/context fields.

It does not prove every Linux ABI or all `clone3` layouts, non-leader-thread exec, `setns` variants, pid reuse under long traces, concurrent directory rename by an untraced process, or crash/truncation behavior beyond the existing parser and storage tests.

Namespace-only unshare has a separate real Linux regression, `tests/namespace_unshare.py`, requiring user and network namespace capability in addition to strace. It enters a user-owned namespace before recording, then checks actual B/A payloads for the shared parent and a private sibling after the shared child successfully unshares NEWNET and changes CWD. The connected group must remain uncertain, the private branch remains known, raw coordinates are retained, and text/JSON self-diff both return 3. The synthetic matrix covers six namespace-only flags, failed counterparts, explicit/implied splits, an unknown symbolic flag, and unfinished overlap; it is not real-kernel coverage of every flag. 该真实 probe 单独运行，不能把主机缺少 namespace 能力记成通过；也不能把一个 NEWNET 正例推广为所有 flags 的实测结果。

五案例审计有彼此独立的历史冻结入口和新鲜采集入口，见
[FIELD_AUDIT_LAYERS.md](FIELD_AUDIT_LAYERS.md)。历史合同与精确 evidence 引用保持不变；新鲜合同验证局部 syscall pair、path 语义、task role、warning multiplicity、reliability 与 ranking。在已测试的新鲜采集中 Cargo 必须零 warning，而历史 Cargo 记录继续保留已保存的 CLONE_FS warning 与 incomplete 状态。这些测试不会升级任何 external-alpha 结论；审计收尾本身不修改模型代码。
