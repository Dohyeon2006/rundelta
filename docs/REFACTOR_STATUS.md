> 本地证据路径说明：本文及本索引所链接历史文档中的 `target/`、本机实验目录和未发布原始日志路径，仅用于说明原本地复现实验位置；这些产物不随公开版提供，不是可访问的公开证据链接。公开可核验材料以仓库实际收录的文件和公开派生合同为准，不能据本地路径推断已发布原始记录。
> Local evidence paths: `target/`, local experiment directories and unpublished logs describe original local reproductions. Their artifacts are not shipped with this public edition and are not accessible public evidence links.

# RunDelta 全量重构状态（2026-09-21 基线，2026-10-07 更新）

本文对应《修复与重构实施方案》和《全量重构任务清单》，记录当前实现状态；它不是新的产品或 schema 合同。语义发生冲突时，以代码、合同测试、README、`FS_CONTEXT_MODEL.md` 和 `REFACTOR_DECISIONS.md` 为准。

## 2026-09-21 兼容重构任务快照

下表与紧随其后的兼容/性能内容保留原 R0～R7 基线，不代表 A18 及显式 v6
扩展后的全部当前格式。当前：默认录制/旧记录自动比较仍冻结在原版本；A18
trace-v1 已验收，显式 v6 实现正在本轮候选验收。详见本文后续有日期的记录与
`REFACTOR_DECISIONS.md`，不以历史“本轮”措辞否定后续版本合同。

| Epic | 状态 | 当前结果 |
|---|---|---|
| R0 / RF-000～030 | 完成 | 冻结文本、JSON、退出码、schema 与隐私基线；建立 accepted/fail-closed 成对回归和统一 full gate。 |
| R1 / RF-100～130 | 完成 | 遏制 hardlink/symlink 写入边界，补齐 SIGSTKFLT，严格验证当前 schema tuple。后续 fd-relative 存储已替代临时遏制层。 |
| R2 / RF-200～250 | 完成 | 引入 `StorageRoot`、`RunWriter`、dirfd 锚定 I/O/删除、分阶段原子提交、typed storage error 与 `RecordSummary`。 |
| R3 / RF-300～350 | 完成 | 引入 task generation/timeline、operation-specific flags、局部 FsContext、pending fork snapshot 及 mount/namespace fail-closed 规则。 |
| R4 / RF-400～450 | 完成 | 引入 `RecordResult`、纯 reducer、`CollectorGuard`、signalfd/pidfd 单线程监督、受监督版本探测和显式软上限。 |
| R5 / RF-500～530 | 完成 | 加载错误中性化，删除生产 `diff -> store`，冻结 raw outcome fallback、变化 outcome 取证和纯 report 展示策略。 |
| R6 / RF-600～620 | 完成 | trace/parser/events/load/list 改为流式路径，合并 annotation 解码，收窄 API，并同步 Rustdoc 与权威文档。 |
| R6 / RF-630 | 按前置条件延后 | 兼容 adapter 必须至少经历一个稳定发布周期后才能删除；本轮只完成依赖/API 审计并移除已无调用的 `signal-hook`。 |
| R7 / RF-700～720 | 基线产品决策后延 | 当时依照 DEC-03～05 不引入 metadata v4、diff v5 或 native pre-runtime shim；后续显式 A18 只承接目标来源/raw 合同，仍不实现 RecorderOutcome 或 shim。 |

DEC-01～05 已全部确定，完整合同见 `REFACTOR_DECISIONS.md`。默认不限制 capture 时长和 raw 大小；显式上限采用软边界，终止 grace 默认 2 秒。取消是 best effort，不承诺目标零副作用。

## 模块和依赖

生产依赖保持单向且无环：

```text
main -> cli, capture, store, diff, report, model
capture -> parse, store, model
parse -> model
store -> model
diff -> model
report -> diff, model
```

`main` 只承担进程边界，`cli` 只声明输入；`model` 无 I/O，`parse` 是增量纯解析与状态归约，`store` 独占持久化，`capture` 编排 Linux 进程生命周期，`diff` 只比较已加载数据，`report` 只展示既有结果。没有新增数据库、网络、全局状态或插件框架；直接生产依赖为 `anyhow`、`clap`、`serde`、`serde_json`、`libc` 和 `sha2`。

## 兼容与性能

- 此基线及当前默认写入为 metadata v3、events v2、finalization marker v1；默认旧/旧 diff 为 v4。A18 显式 trace-v1 使用新 tuple/自动 diff v5，显式 v6 只扩展报告；历史记录不重写、不以新 parser 追溯解释。
- 四态 diff 合同保持 0/1/2/3，文本与 JSON 使用同一计算结果；每侧 evidence sample 仍有界。
- `parse_with_cwd` 和 `RunWriter::read_trace` 仅作为 RF-630 前的兼容/测试 adapter 保留，生产 capture 已使用逐行 parser 与流式 trace reader。
- 同源 benchmark 的 100k、400k、1.2m 行场景中，流式 parser 的峰值 RSS 分别下降 10.17%、10.63%、10.76%，事件、warning 和行数逐字等价；详见 `performance/rf600-streaming/README.md`。

## 本轮验证

本节原始 R0～R6 结果是 P1 发布前复审前的快照；最终提交须另外通过下文的
P1 回归与候选暂存区验证，不能只凭先前 13 项通过宣称可以提交。

- `cargo fmt --check`：通过。
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`：通过。
- `cargo test --workspace`：162 个单元测试和 4 个合同测试通过；8 个 Linux acceptance 测试按默认配置登记为 ignored。
- `python3 -B tests/full_gate.py`：13 项通过，包括 26 个 completion 合同、4 个 CWD、8 个 FsContext、1 个真实 mount namespace、真实 capture、9 个 reliability、17 个 runtime-boundary 和 12 个 storage CLI 用例；唯一失败为当前工作树非干净，因此不能把它声明为同一 clean revision 的 package gate。
- 新鲜五案例审计器回归 25/25 通过；用当前二进制重新采集 Python、pkg-config、Git、GCC、Cargo 后，独立语义审计 5/5 通过，关键线索、任务角色、精确 warning、排名、可靠性和本次 raw evidence 坐标均符合预登记合同。
- 从当前显式发布白名单复制相同源码字节到一次性干净树后，`cargo package --locked --offline` 通过；生成的 15 文件 crate 又通过 18 个负例、精确 inventory、源码逐字比较、MIT metadata/text 和多层解码隐私扫描。
- `cargo doc --workspace --no-deps --document-private-items`：通过。

## P1 发布前修复与提交闭包

本轮先在修复前实现失败回归：`pidfd_open=ESRCH` 与 `wait=ECHILD` 的受控
PID 复用替身不能被清理信号杀死；继承 `SIGCHLD=SIG_IGN` 时，版本探测前必须
拒绝无法证明可回收性的环境。低 fd 上限下必须保留全部标签，扫描目录或 metadata
发生实际 I/O 错误时必须拒绝 reserve/record，且不运行目标或创建记录目录。

安全门禁由 crate 根的 `deny(clippy::undocumented_unsafe_blocks,
unsafe_op_in_unsafe_fn)` 固定。每个 unsafe 块说明 Linux FFI 的指针、初始化、
fd 所有权或 pre-exec 安全前提；不以 lint 通过代替对注释准确性的审阅。

普通 `tests/full_gate.py` 继续要求工作树对应干净提交。提交前使用
`python3 -B tests/full_gate.py --candidate-index <isolated-index>`：从显式隔离 index
生成固定 tree，在临时目录对同一 tree 运行原 13 项门禁与 package/privacy 检查，
前后核对源码字节、权限、index tree 和 HEAD。此模式不创建 commit，不接受用户的
默认 index，不把工作树的额外文件带入测试。候选中缺少验收资产、fixtures 或有陈旧
字节时，`tests/check_release_closure.py` 拒绝进入门禁。

修复前，两个 PID 替身测试均失败，SIGCHLD 忽略环境错误启动了版本探测；
70 条记录配合 fd 上限 64 会遗漏 11 个标签，重复录制与读取失败路径也未拒绝。
启用 unsafe deny 后原代码产生 42 个缺注释错误；HEAD 和旧 tracked-only 候选均
未通过验收闭包。修复后，capture 单测 45/45、store 单测 44/44、隔离二进制
runtime-boundary 18/18、storage CLI 16/16、闭包单测 7/7 和 gate 自测已通过。
完整固定候选 tree 的 Linux/package 结果以最终提交前 gate 输出为准。

未跟踪文件按来源处理如下；目录项表示该目录内的完整固定 fixture 清单，不包括临时
target 工件：

| 来源 | 文件 | 提交处理 |
|---|---|---|
| 用户既有文件 | `.idea/RunDelta.iml`、`.idea/modules.xml`、`.idea/vcs.xml`、`AGENTS.md` | 排除；已暂存的 `.idea/.gitignore` 同样排除但保留用户暂存状态。 |
| R0 必需合同资产 | `tests/contract_baseline.rs`、`tests/fixtures/contract_baseline/` | 纳入，包括当前/历史 schema、raw 合成证据、golden 和 baseline manifest。 |
| Linux 验收闭包 | `tests/runtime_boundary.py`、`tests/mount_namespace.py`、`tests/full_gate.py`、`tests/fixtures/runtime_boundary_probe.c`、`tests/fixtures/pthread_eagain.c`、`tests/fixtures/prctl_fail.c`、`tests/fixtures/version_backend.py` | 纳入；它们是验收入口的实际依赖。 |
| 本轮闭包回归 | `tests/check_release_closure.py`、`tests/test_release_closure.py` | 纳入；候选缺件/陈旧字节必须失败。 |
| 本轮重构文档 | `docs/REFACTOR_DECISIONS.md`、`docs/REFACTOR_STATUS.md`、`docs/performance/rf600-streaming/README.md` | 纳入；RF-600 基准文档由本轮生成，不是用户既有 A3 性能材料。 |

用户已有的 `README.md` 混合修改、`docs/performance/a3-completion-check/README.md`
和 `docs/external-alpha/attribution/probe.c` 改动不进入候选提交。已有历史基线文件保留
原提交内容；本轮不覆盖它们，也不公开导出任何内部材料。

## 2026-09-22 最小可靠性加固

本节限定生产改动为 `capture` 的信号恢复和 `store` 的 writer 生命周期；不拆模块、
不改 parser/diff/report、不引入生产依赖、全局状态或 schema。公开导出是后续独立
审查，本轮不修改公开仓、remote、tag 或发布配置。

- 冻结后不再消费 signalfd；恢复阶段自身也冻结观察，失败重试不得吞掉新 pending
  信号。迟到信号按原 disposition/mask 处理，目标 metadata 不被重新解释。
- `RunWriter::write_metadata(&mut self)` 只允许一次初始 running metadata 发布。
  实际 I/O 失败后禁止重试，以免把已发布但持久性未知误当成未写入。
  `finish(self, ...)` 成功或失败均消费 writer；锁与 fd 覆盖整个提交及清理过程。
- 回归先验证旧代码失败：冻结后信号消失、未初始化完成、重复初始写入。提交屏障
  fixture 仅用于测试，不提供生产后门。覆盖三种控制信号、默认/忽略/阻塞、提交
  成功/失败与无信号对照，并检查 marker、临时文件和锁释放；Rust 补恢复重试和
  编译期 receiver 合同。
- `tests/acceptance_support.py` 统一六个 unittest 验收入口的退出策略，开发模式
  保留原 skip 行为；`--release` 模式拒绝空套件、skip 或 expected failure。
  不解析终端摘要来猜测 unittest 结果。普通 Cargo 测试包含门禁策略自测。
- 严格模式核对编译后的八项 ignored acceptance 登记，防止 Cargo 精确过滤匹配
  零测试仍成功；汇总拒绝缺项、重复项、非 passed 状态。隔离候选还必须通过闭包与
  字节不变检查。验收 helper 和 finalization barrier 均纳入候选资产闭包。

工作树带有用户修改时，用显式白名单填充独立 index，然后运行：

```text
python3 -B tests/full_gate.py --release --candidate-index <isolated-index>
```

`--release` 不能与 `--core-only` 或 `--self-test` 组合。开发者仍可单独使用它们，
但不能把允许 skip 的开发结果称为完整发布验收。此节记录实现与验证要求，不用
历史测试数量代替本次候选 gate 的实际输出。

## 保留边界和后续债务

- RF-630 adapter 删除、结构化 RecorderOutcome 和 native pre-runtime shim 仍未实施。A18 trace-v1 只增加目标结果 provenance 与 raw 完整性；outcome 全映射与样本省略数由本轮显式 v6 报告承接，验收结果须独立留证，不能以 A18 PASS 替代。
- 软大小上限由约 50 ms 轮询和解析前最终 `fstat` 实现，不是精确配额；取消和 collector tree 终止也不承诺瞬时、零副作用或任意 daemon/ptrace 脱离场景。
- 尚无断电/文件系统故障注入、复杂特权 namespace、不可中断内核等待的完整证明。
- 后续可再补 record 级最终 `fstat` 故障注入、真实 strace byte-limit 目标树回收、慢版本探测不消耗 duration，以及长运行默认无限制的专项 acceptance；已有纯 reducer、监督错误、duration 真实回收和 byte-limit 边界测试覆盖共享生产路径。
- 提交前以固定候选 tree 重跑完整 gate；本地提交与公开发布相互独立，不推送、不修改远端。

## 2026-10-07 内部审查整改

本轮以内部审查 A01～A19 的可复现反例为入口。实现与验证分开记账：下表说明
代码和回归入口，实际通过结论须来自同一个固定候选树的 full gate 原始日志。
未修改 metadata v3 / events v2 / marker v1 / diff v4，也未重写历史证据、五组冻结
输入或其预期。生产依赖仍为既有六项；纯领域合同移入 `model`，storage 与 diff
消费同一校验，未引入 `diff -> store` 或 `model -> I/O`。

| 审查项 | 实现 / 设计依据 | 正例与反例入口 |
|---|---|---|
| A01 | capture rollback 先恢复 disposition；失败保持控制信号阻塞并合并诊断 | runtime_boundary setup failure + pending HUP + rollback success/failure |
| A02 | store 读写相同容量/JSON 节点预算、可失败缓冲分配；资源限制为 unavailable | storage_cli 健康行与大输入隔离、合法大行、原 RLIMIT_AS 反例 |
| A03 | model 当前事件形状、完整性事实校验；store writer/reader 同时消费 | current_shape / pathless_current、storage_cli 自洽 checksum 非法事件 |
| A04 | clone3 专属 exit_signal 条件不施加给 legacy clone | parser ABI pairs；fs_context 真实 CLONE_PARENT 与 clone3 accepted/fail-closed |
| A05 | 普通完整 gate 和候选 gate 均先固定 Git tree，再在隔离树检查 | gate self-test clean A→B、字节变更、额外 build.rs、快照身份检查 |
| A06 | 独立 package 外部输入校验改用显式 require | 同一合法/非法包在普通 Python、-O、PYTHONOPTIMIZE 三种模式 |
| A07 | 独立 raw→normalized 与 normalized→raw 双向观察账；噪声排除有明确范围 | test_fresh_field_audit 遗漏/错误/unfinished/resumed；保存 accepted 记录只读重审 |
| A08 | check/gate 截止时间、WNOWAIT 私有进程组所有权与回收；示例 finally 清理 | gate self-test owned hang；real_capture readiness/cancellation |
| A09 | acceptance_support 单一 strace resolver，最终绝对路径传给各套件 | gate self-test bundled/system/explicit/absent 与无效显式路径 |
| A10 | audit_oracles 为必需唯一 gate 项，注册全部 test_*.py | unittest discovery、严格 required inventory、缺项/失败控制 |
| A11 | model 纯 metadata/event/run 合同；有效比较 fixtures；unfinalized 仅保留既有 withholding 例外 | model contract_tests、diff direct DTO rejection、storage compatibility/goldens |
| A12 | ChangeKind 决定排序，英文 category 保持 diff4 兼容拼写 | 每个 semantic kind 与文案变化对照；冻结 rank/order/text/JSON |
| A13 | StartupGate / TraceReadPlan 表达启动和读取阶段，未知长度不能被后续观察覆盖 | workflow_phase_tests、既有 reducer matrix 和 runtime boundary |
| A14 | scope !Send/!Sync；入口完整检查 procfs task 清单 | compile constraint；single-thread accepted / extra pthread rejected、全部信号状态未改 |
| A15 | ParsedObservation 绑定 event/action，无缺项默认 None | mixed transition 顺序/坐标 golden 与完整 FS 回归 |
| A16 | 拒绝内部未转义引号；只在字符串外识别缩略标记 | escaped/hex/quote/literal ellipsis 正例与 malformed 反例 |
| A17 | 注释说明真实合同；恢复测试比较全部 action flags/mask/thread mask 并主动扰动非控制位 | signal_scope restoration success/retry/child helper；schema golden |
| A18 | 已实现显式 trace-v1：metadata4/events3/raw manifest1/marker2/diff5；默认版本与历史读取冻结 | parser/capture generation 与选择回归、model pure contract、store同流收据与独立损坏、storage_cli新旧/反例；完整候选 gate 另行留证 |
| A19 | 新 gate 保存 source archive/tree、driver/tool/version/hash、选定环境、binary 和逐项日志 | 固定候选 summary/inputs/log SHA；源码前后检查；历史性能不补造复现证据 |

结构化存储的操作预算：metadata 8 MiB、marker 64 KiB、JSONL 单行（含LF）8 MiB、
events 文件 256 MiB、最多 1,000,000 事件、每份 JSON 最多 65,536 值（键、字符串、
容器、标量）。限额与 raw capture 软上限分开；超限或缓冲分配失败不判 corruption。
这些有限预算不等于任意 RLIMIT 下均能恢复 OOM；目录库存及 list summaries 总量
仍随记录数增长。相关对外行为在 `CRATE_README.md` 同步中英说明。

普通 full/release gate 现在先要求 clean revision，再导出固定 tree；工作树有改动时
继续用独立 index 候选模式。`--check-timeout` 默认 600 秒，`--gate-timeout` 默认
3600 秒，可按环境显式调整有限正数。deadline 不覆盖不可中断的内核操作保证；
私有进程组回收不选择任意外部 PID，也不承诺覆盖主动 setsid 脱离的后代。
`--evidence-dir` 指定新的本地目录；默认写到 target/gate-evidence。产物含私有
源码与环境路径，不能直接公开。source.tar、inputs.json、binary.json、summary.json、
实际受测 rundelta-tested 及每项日志构成本次验收账。工具探测在实际 snapshot cwd
执行，保存版本输出原字节；rustup 入口和其选择的 rustc/cargo 分别存摘要。普通和
复合检查的超时前 stdout/stderr 均保留，辅助 Git 超时写入失败 summary。source
archive 是重放输入，工具版本/hash 是匹配条件，
不是工具自动安装或跨平台兼容承诺。缺 host capability 的开发 skip 仍不能当 release
通过。新增正常固定快照和所有反例共同构成门禁的验证依据。

测试工具在 spawn 前拒绝可检测的非默认 SIGCHLD；waitid 报 ECHILD 后不按原数字
进程组 ID 发信号。生命周期仍要求无竞争 reaper 和原生 SA_NOCLDWAIT 变更，Python
handler API 不证明这些前提。文件日志还没有总输出配额；验收也未隔离工具链全部
动态库、wrapper 和 Cargo 依赖缓存，不把当前复现条件说成完全 hermetic 的构建。

A18 后续整改按 `REFACTOR_DECISIONS.md` 的显式新版本合同实现：默认 marker1 仍只
证明结构化完整性，不覆盖 raw；marker2 绑定 raw manifest，只有同解析流 Parsed
收据经 raw 字节数、SHA-256 与逻辑行数校验后才得到独立 raw verified。两者都不
认证来源或保证断电恢复。旧记录不升级、不重解析。旧 RF-600 性能
材料保留其原复现限制；本轮没有重测性能或补造丢失的历史 harness。

本轮局部多线程 guard 只证明入口观测，调用者仍须在 capture 生命周期维持无并发
线程创建、信号 disposition 修改、竞争 reaper 的条件。固定候选树实际 full gate
结果及 A01～A19 状态账保存在本地审查整改产物，不以本表代替实测 PASS。


## 2026-10-07 A18 显式证据协议整改

本节是实现状态，最终通过结论以同一固定候选树的本地原始日志为准。模块边界
仍为 capture → parse/store/model，store/parse/diff/report → model；model 的
版本、代次、provenance 与收据检查没有 I/O，diff 消费已加载的分项事实并计算
四向状态，report 只呈现同一结果。新增 raw 读取仅用于验证同一次归一化产生的
收据；没有新生产依赖、trait 框架、global state 或反向模块依赖。

store 的既有 cfg(test) identity_tests 机械抽取到私有同模块文件，以在保留生产
注释和原有 200 kB 文件预算的同时降低文件集中度；抽取前后原始测试 body SHA
另存本地证据。此文件进入开发验收源码闭包，但不加入公开包 allowlist。
旧 tuple 的 list 以 validate-only 模式流式只累积事实；trace-v1 marker-v2 列表
分支临时收集一条记录的事件，以校验完整保存的引用/代次，构造 summary 时释放
event vector，返回的列表行不保留事件。现有逐记录资源预算仍适用；该选择增加
新格式 list 的临时内存成本，索引/summary 总量仍随记录数增长，不宣称任意分配
失败或资源限制下都可恢复。

新路径错误优先级固定为：marker 头部/ID、绑定 metadata/events/manifest 字节
校验、metadata 重读/解码、事件解码/计数/合同、版本 tuple、manifest 版本/DTO/
跨字段合同，最后 raw 独立校验。结构化绑定失败仍为加载错误；raw 独立失败
保留已验证的结构化 facts，diff 返回 2 且不产生普通变化结论。

历史第一轮 A01～A17/A19 的固定候选验收与 A18 后续验收分开保留，不能用前者
冒充新版本已通过，也不能改写原始审查报告或失败反例。新协议使用说明保持中英
双语，CLI/JSON 字段仍为英文。未推送、未导出公开历史或内部证据。

独立复查又发现 metadata4 可在保持退出引用为 0 的同时篡改目标数值为 7；因此
trace-v1 加入固定纯退出词汇/数值合同，并使新 capture 分支与 reader 共用。legacy
分支不改 decoder；两者共享原有 capture reducer 政策，没有反向依赖或 OS 查表。
新增正反例覆盖数值矛盾、显式 signal 事实、未知信号、固定 RT 边界及 legacy 保留。

同一 identity v1 合同还要求：已保存 first-successful exec 与同代 exit 时不能
删除目标 provenance/raw outcome；Unintroduced/Reappeared/LiveCollision 的
保存 actor 保留 context_uncertain，记录保留 warning 且不能声称 complete。
校验只消费已保存 DTO，不复推 filesystem sharing 或固定自由文本 warning。

A18 最终固定候选树为 `c092a500f9be2aece76ded3bede19f797c937efd`，完整 release
gate 19/0/0，source.tar 独立空 Cargo cache 重建 18/0/0；两者均未 skip。其 228
Rust 单测、四组冻结 golden、八套 Linux 验收和原始日志在
本地 A18 整改账（原本地路径 `../target/a18-remediation/20261007/remediation.json`） 对应。
这些是 A18 的验收事实，不替代后续源码树的验证。

## 2026-10-07 显式 v6 outcome 引用映射

本轮 implementation 进入候选验收，最终结果由新的固定源码/二进制/输入/原始日志
账另行确认；本节不预写 PASS 数量。显式 `diff --report-version 6` 增加
`outcome_event_references_v1` 合同；默认 v4、自动/显式 v5、录制与存储 tuple
保持原合同。v6 继承 v5 raw/provenance 可信要求，旧证据不升级、不重解析。

diff 为每项 file/exec 变化按 outcome 保存原 event index/坐标的全量映射；三条
旧样本另存 sample index、展示数和省略数。重复观察保留，但不变成跨 run 比较
身份。目标变化使用可用的保存 target provenance；其他变化明确 metadata field
来源，unfinalized 输入明确 withholding，不能把无坐标说成零事件。全量范围仅为
变化键的已保存 normalized events，不承诺所有 raw syscall 或未变化键。

显式 v6 先执行现有 DTO 合同校验，再排除合法 unfinalized 侧的全部 file/exec
事件，令其不贡献聚合键、outcome/change、平面样本或全量映射；也覆盖内部合法
非空 metadata-v3 DTO。CLI loader 原本就扣留事件；准入规则不修复、不重写
输入。另一侧自己的已加载观察和 metadata 比较仍保留，withheld 引用为空、
数量为 null、状态仍为 3；非法 DTO 先拒绝为 2，unverified 事件仍展示。
unfinalized 侧的 Exit status 平面样本也清空。v4/v5 原行为保持，顶层保存的
target_provenance 仍可保留，但该侧 quality 为 unverified，不能作为 finalized
映射。不能概括为所有 v6 输入删去新字段后与 v5 完全相等。

report 只序列化上述 facts 并展示 source/count/sample/omitted，保留前五/折叠
和 JSON 全量 changes/references；不重算数量、可靠性或状态。依赖仍为 diff →
model、report → diff/model，无新 I/O、生产依赖、trait 或 global state。owned
映射与完整 pretty JSON String 增加线性内存/输出成本；不承诺任意 OOM 恢复。
RF-630 稳定发布周期、RecorderOutcome、native shim、断电、工具链 hermetic、
输出配额仍是独立前提或技术债；本轮不顺带实现或削弱其边界。

验证入口为 diff/report colocated 正反回归、storage CLI v6/旧格式/损坏矩阵、
固定候选 full release gate 与严格 Rustdoc。冻结五例和旧 schema golden 不变，
验收事实不能借用 A18 的通过结果或覆盖旧失败记录。

## 2026-10-07 两项 P3 合同修正

本轮仅修正列表内存生命周期说明与显式 v6 的合法 unfinalized 事件准入边界；
不拆分 capture、不修改 model/storage 逻辑或格式、预算、依赖方向。列表 Rustdoc
与双语说明按实际 reader 分支区分旧 validate-only 和 trace-v1 临时 collect；
v6 的 DTO 校验先于 withholding，因此 malformed 输入不会因为扣留而变成可用。
metadata/provenance 保留，unverified 与 unfinalized 不合并，v4/v5 原行为冻结。

This cycle corrects only the list-memory lifetime description and explicit v6
admission of valid unfinalized observations. It neither splits capture nor changes
model/storage logic, formats, budgets or dependency direction. List Rustdoc and
bilingual contracts distinguish old validate-only reads from trace-v1 temporary
collection. V6 validates DTOs before withholding, so malformed input cannot become
usable through withholding. Metadata/provenance remain; unverified and unfinalized
are distinct, and v4/v5 behavior is frozen.

合法非空 unfinalized metadata-v3 的失败回归先确认两侧均通过现有 DTO 校验，
再暴露 withheld 映射与非空平面样本/outcome 矛盾。RED tree 与原始日志保存于
`target/p3-contract-remediation/20261007`；修复后的固定源码验收另行记账，
本节不预写 PASS，也不覆盖此前 v6 或 A18 凭据。

The legal nonempty unfinalized metadata-v3 regression first validates both DTOs,
then exposes the contradiction between withheld mapping and nonempty flat
samples/outcomes. Its RED tree and original logs live in
`target/p3-contract-remediation/20261007`. Fixed-source verification of the fix
receives a separate receipt; this section does not predeclare PASS or overwrite
earlier v6 or A18 evidence.

## 2026-10-07 capture 第一批：completion 提取

以固定树 `64187cf3c114be35125b7ed4253d5f55c674dfac` 为本轮起点。第一批将目标
选择、generation 完成分析和 provenance 投影迁到私有 `capture::completion`，
仅依赖 parse/model；字段私有，父模块通过根退出索引、缺失 generation 切片和
provenance 方法读取结果。算法、事件顺序、索引和证据坐标不改变。纯测试随代码
迁移，涉及采集归约的测试保留在编排边界，不给 completion 添加反向依赖。

This first batch starts from the fixed tree above. Private `capture::completion`
owns target selection, generation completion analysis and provenance projection,
with parse/model dependencies only. Private fields are read through narrow
methods. Algorithms, event order, indices and coordinates remain unchanged;
pure tests move with the implementation, while reduction integration tests stay
at the orchestration boundary.

退出解码、事实归约、Linux 监督及 record 生命周期在本批保持原位置。新增源文件
逐项审查包清单，不扩大扫描例外或资源预算。固定源码验收、独立重建和逐行迁移
账另存 `target/completion-refactor/20261007`，本节不预写通过结论。第一批验收
完成前不开始第二批实现。内存只保存基线，完整事件/引用/JSON 物化成本仍是技术债。

Exit decoding, fact reduction, Linux supervision and the record lifetime keep
their existing locations in this batch. The new source file receives explicit
package review without scanner exceptions or larger budgets. Fixed-source
acceptance, independent rebuilding and line migration receipts live separately
in `target/completion-refactor/20261007`; this section does not predeclare success.
Implementation of batch two starts only after batch one is accepted. Memory work
records a baseline only; full event/reference/JSON materialization remains debt.

## 2026-10-07 capture 第二批：类型化归约与退出译码

第一批固定树 `099bd0b37be5b44541e29d8e21473a9b6384336f` 已独立验收：完整门禁
19/0/0、空 Cargo cache 源码包重建 18/0/0、严格 Rustdoc 通过；保留输入的156组
CLI 对比逐字节一致。凭据在 原本地复现实验目录 `target/completion-refactor/20261007/` 内的验收账（未公开，不随公开版提供），
不是第二批的通过凭据。旧账与冻结输入不回写。

Batch one was accepted at the fixed tree above: full gate 19/0/0, independent
source rebuild with an empty Cargo cache 18/0/0, strict Rustdoc passed, and 156
preserved CLI comparisons were byte-identical. Its separate acceptance receipt
is not evidence that this second batch passed; previous ledgers and inputs remain.

第二批将 `reduction` 限定为已观察事实 → 状态/完整性/CLI/metadata 投影，只依赖
model 与标准库。`platform_exit` 解释现有 legacy host ABI、ExitStatus 与 trace-v1
固定词汇，依赖 reduction/model 及已有 libc/std 适配；纯归约不接收 decoder 回调，
不依赖 Linux、process、signal 或 store。目标原文与类型化退出事实绑定在同一个
可选观测中：Unknown 仍保存原文，None 才代表未观察。

The second batch limits `reduction` to observed facts → state, completeness,
CLI and metadata projection, depending only on model and the standard library.
`platform_exit` adapts the existing legacy host ABI, ExitStatus and fixed trace-v1
vocabulary through reduction/model and existing libc/std. Pure reduction accepts
no decoder callback and depends on no Linux process/signal or storage code.
A single optional observation binds raw target text to its typed outcome:
Unknown retains raw text; None means no observation.

原状态优先级、warning 顺序、退出数值/显式 signal 区别与既有矩阵完整事实对照
保持。生产结果字段私有，通过窄方法读取/投影；测试 snapshot 保留原 Eq 的十项
字段（limits 中两项事实均保留）。本批不改变 SignalScope、完成选择或存储流程。
新的固定验收与迁移账在 `target/reduction-refactor/20261007`，本节不预写通过。

State priority, warning order, explicit signal/code separation and complete
matrix comparisons remain. Production fields are private and read/projected
through narrow methods; test snapshots preserve all ten fields in the previous
Eq, including both limit facts. SignalScope, completion selection and persistence
keep their workflow. New fixed-source acceptance and migration receipts live in
`target/reduction-refactor/20261007`; this section does not predeclare success.

## 2026-10-07 capture 第三批：Linux 监督与编排

第二批固定树 `f38cc86480b447b4c03cf2508849dd708c0aa8f6` 已独立验收：完整门禁
19/0/0、空 Cargo cache 源码包重建 18/0/0、严格 Rustdoc、156组保留 CLI 对比
通过。逐行账区分32,700继承、891搬运、492候选 hunk，正常/-O输出相同；凭据
在 原本地复现实验目录 `target/reduction-refactor/20261007/` 内的验收账（未公开，不随公开版提供）。这些不替代第三批验证。

Batch two was accepted at the fixed tree above: full gate 19/0/0, independent
empty-cache source rebuild 18/0/0, strict Rustdoc and 156 preserved CLI byte
comparisons passed. Line receipts distinguish 32,700 inherited, 891 migrated and
492 candidate-hunk lines, with identical normal/-O output. Its receipt does not
replace acceptance of this third batch.

第三批由 `supervision` 私有拥有 child/pidfd、waitability、信号动作/mask、管道、
probe/formal collector 和清理权限；`policy` 保存共享限额配置，capture facade
重导出现有 CaptureLimits 接口。监督生产代码依赖 policy/reduction/platform_exit
及已有 std/libc/anyhow，不访问 store、parse 或父编排内部。旧存储锁回归随资源
测试迁移，仅 cfg(test) 保留 store 集成边界，不据此开放生产资源字段。

In batch three, `supervision` privately owns children/pidfds, waitability, signal
actions/masks, pipes, probe/formal collection and cleanup authority. `policy`
holds shared limits and the capture facade re-exports the existing CaptureLimits
API. Production supervision depends on policy/reduction/platform_exit and the
existing std/libc/anyhow, without storage/parser or parent-workflow internals.
The prior storage-lock regression retains a cfg(test)-only integration boundary;
production resource fields remain private.

trace 长度由函数端口提供，保留原观察顺序、严格超过边界、首轮锁定和最终读取前
长度检查；失败仍是独立 supervision error。无 byte limit 时不得调用端口。deadline
仍锚定于 Command 构造前，原 schedule 早期错误与 spawn/attach 的 collector
failure 分开。probe 与正式启动的 cancellation 观察点不合并。

A function port supplies trace length while preserving observation order,
strict excess, first-violation latching and the final pre-read check. Failure is
a separate supervision error; disabled byte limits perform no port call.
Deadlines remain anchored before Command construction. Early schedule errors
remain distinct from collector spawn/attach failure, and probe/formal startup
cancellation observation points are not merged.

同一 SignalScope 仍由 record 持有，贯穿解析、freeze、归约、writer.finish 和外层
restore 四臂结果合成。已提交但恢复失败继续返回独立工具错误，不回写 capture
facts，也不自动重跑目标。新增私有故障探针在 marker 成功发布后注入恢复失败，
涵盖 legacy/trace-v1；不修改冻结 fixture 或给生产增加测试开关。实际新树验收、
独立重建与来源账另存 `target/supervision-refactor/20261007`，本节不预写 PASS。

The same SignalScope stays owned by record through parsing, freeze, reduction,
writer.finish and the outer four-way restoration result. A committed record plus
restoration failure remains a distinct tool error, without rewriting capture
facts or rerunning the target. A new private probe injects restoration failure
only after marker publication for legacy/trace-v1; frozen fixtures and production
code receive no test switches. Fixed-source acceptance, independent rebuilding
and line ancestry live separately; this section does not predeclare PASS.

新恢复测试先经 libc 显式设置并查询既有控制信号动作，以该完整动作域作为初态，
逐项比较 handler、flags 与 mask。旧二进制的失败探针还保存了 exec 继承初态
flags 为零、恢复后 glibc 加入 SA_RESTORER 的 ABI 观察；不屏蔽未知 flags，
不修改生产 syscall，也不宣称任意内核动作位能逐位还原。

The restoration probe explicitly sets and queries the existing control actions
through libc before recording, then compares every handler, flag and mask member
in that established domain. Failed baseline attempts retain the ABI observation
that glibc adds SA_RESTORER when restoring an exec-inherited zero-flags action.
Unknown flags are not masked, production syscalls are unchanged, and this does
not prove bitwise restoration of arbitrary kernel action states.

本轮没有改 JSON 输出或引用存储。20,000事件/记录的合成 legacy 读取基线绑定
64187cf3树及旧二进制，记录整 CLI RSS，不是独立 capture 分配量，也不构成优化
结论。同步 spawn handshake 不受监督硬 deadline 覆盖、cleanup wait 无用户态硬
上限、单线程/独占 reaper 前提、完整事件/引用/pretty JSON 的线性内存成本保留。

JSON/reference storage is unchanged. The synthetic 20,000-event legacy read
baseline binds the original tree/binary and measures whole-CLI RSS, not isolated
capture allocations or an improvement. Spawn handshake lacks a supervision hard
deadline, cleanup wait has no userspace hard bound, single-thread/exclusive-reaper
premises remain, and full events/references/pretty JSON retain linear memory cost.

## 2026-10-07 内存成本治理候选

治理基线为第三批最终验收树 `f8ef9eaadd421d55ae85e92ee3e21a7f5212bdc7`。
先核源码/权限、HEAD/默认索引与历史清单，再测独立诊断进程及真实 CLI：三个
规模、四种变化分布、各串行三次。诊断 allocator 的直接计数与包含准备的绝对
RSS 分开解释；不相减两个进程的峰值来归因。

The baseline is the final accepted batch-three tree. Source bytes/modes,
HEAD/default index and historical manifests are checked before three sizes,
four change distributions and three serial repetitions in private diagnostics
and the real CLI. Direct allocator counts remain distinct from absolute RSS
including preparation; subtracting two process peaks cannot attribute a phase.

依据完整 JSON String 的实测分配，仅改 diff JSON 的流式呈现与 main 输出边界，
保留完整引用。新增序列化错误、部分/中断/零字节写入、BrokenPipe、其他写入及
flush 失败单测，以及真实 CLI 四态/版本/跨缓冲区反例。固定树门禁、输入/输出
字节、冻结五例、白名单/隐私、逐行来源及独立空缓存重建的结果另存新账；本节
不将候选工作预先称为完成。

Measured complete-output allocation motivates one change: streamed diff JSON at
the presentation/process boundary, preserving full references. Tests cover
serialization faults, partial/interrupted/zero-byte writes, BrokenPipe, other
write/flush failures and real CLI versions/four states/output exceeding the
buffer. A fixed tree validates inputs/output bytes, the frozen five,
allowlist/privacy rules and line ancestry, followed by an empty-cache rebuild.
Receipts record actual completion separately; this section does not predeclare it.

完整事件/引用、v6 验证/聚合中间容器及 list/show JSON 缓冲的成本保留。
capture 的同步 spawn handshake、无用户态硬上限 cleanup wait、单线程/独占
reaper 等债务保留。不加输出配额、不改 capture、不承诺任意 OOM 恢复，不发布。

Full events/references, v6 validation/aggregation containers and list/show JSON
buffers retain memory cost. Capture's synchronous spawn handshake, cleanup wait
without a userspace hard bound and single-thread/exclusive-reaper premises remain.
No output quota, capture change, arbitrary OOM recovery or publication is added.

## 2026-10-07 diff 聚合成本治理候选

基线为 `fe70b93b822a57948363c1510c40fad6a7e7e32d`。完整加载、v6 比较和真实
CLI 复用封存输入，三个规模、四种分布、各串行三次。私有诊断另按验证、聚合
树/索引、key 并集、抽样和完整映射的实际分配操作归因，并核对诊断与普通构建
总计一致；RSS 是独立进程绝对峰值，不相减。初版协议的读入/缺失指标防护不足
及加强协议重跑均保留，不能把两次测量累计成独立完整验收。

The accepted baseline and sealed inputs cover full loading, v6 comparison and
the real CLI at three sizes, four distributions and three serial repetitions.
Private diagnostics attribute actual allocation operations to validation,
aggregate trees/indices, key union, sampling and full mappings, checking totals
against regular diagnostics. RSS is an absolute process peak, never subtracted.
Initial read/missing-metric guard limitations and the strengthened rerun remain
recorded; repeated measurement does not constitute two complete acceptances.

只以借用有序合并替代比较 key 的临时 BTreeSet，并保持聚合/引用生命周期。
旧算法先冻结 21 份版本/状态/文本/JSON 快照，新增交错、重复、空侧及先校验再
扣留回归。候选同条件测量、156 组 CLI 对照、完整固定树门禁、严格 Rustdoc、
白名单/隐私检查、来源续账和独立空缓存重建另存新账；本节不预判最终收益或
验收完成。新增诊断和凭据工具只在私有 target 内，不进入生产 CLI 或包白名单。

Borrowed ordered merging replaces only the temporary comparison-key BTreeSet;
aggregate/reference lifetimes remain. The old algorithm freezes 21 version,
status, text and JSON snapshots, with interleaving, repetition, empty-side and
validation-before-withholding regressions. Matched candidate measurements,
156 CLI comparisons, full fixed-tree gates, strict Rustdoc, package/privacy
checks, line ancestry and an empty-cache rebuild receive a new ledger, without
predicting benefit or acceptance. Diagnostics and receipt tools remain private
target assets outside the production CLI and package allowlist.

内层 outcome 树、完整事件和索引、完整 v6 引用、展示向量及 list/show JSON
缓冲仍有增长成本。未同时优化聚合表示或提前释放；capture 既有生命周期债务
保留。无任意 OOM 恢复、整体恒定内存、跨环境收益或全部源码合理性的证明；
未发布、未推送。

Inner outcome trees, full events/indices, complete v6 references, presentation
vectors and list/show JSON buffers retain growth costs. Aggregate representation
and early release are deferred, as are capture lifetime debts. No arbitrary OOM
recovery, constant total memory, cross-host improvement or all-source correctness
is proved. There is no publication or push.

## 2026-10-09 diff 内层 outcome 聚合成本治理候选

以 `c73bccb76f1de9215651083ac49b67cebf3f7f94` 为直接基线，先完成三种表示、
二十四种分布的受控构造实验及基线加载/比较/呈现/CLI 测量，再采用私有
Empty/Single/Multiple 表示。单 outcome 不再分配独立内层树，第二种 outcome
移动完整索引 Vec 到有序树；保存索引、坐标、重复观察、顺序和四态合同不变。
原有序合并、样本、映射、可靠性和校验算法不改；样本 lookup 的两个调用仅移除
冗余显式解引用，以通过严格 Clippy，原失败门禁另存而不覆盖。

The direct baseline is `c73bccb76f1de9215651083ac49b67cebf3f7f94`. Controlled
construction compares three representations over 24 distributions, followed by
baseline load/compare/render/CLI measurement. A private Empty/Single/Multiple
representation avoids a separate inner tree for one outcome and moves its full
index Vec into an ordered tree when a second outcome appears. Saved indices,
coordinates, repeats, order and four-way status remain; existing merging,
sampling, mapping, reliability and validation algorithms stay unchanged. Two
sampling lookup calls omit redundant explicit dereference for strict Clippy;
the actual failed first gate remains separately preserved.

原算法已实际编译通过新增六个边界测试并冻结三十九组输出；新增表示还需在独立
候选上通过回归、实际编译执行的错误变体、全规模前后测量与完整固定树门禁。
只将完成的检查记作验收，私有诊断不算产品验收。工具、原始日志、逐行来源、
独立重建与清单另续账，不覆盖历史记录或冻结五例，不改打包白名单与隐私规则。

The old implementation actually compiled and passed six new boundary tests,
freezing 39 outputs. The candidate still requires regressions, actually compiled
and executed faulty variants, all-scale matched measurements and full fixed-tree
gates. Only executed checks count as acceptance; private diagnostics are not
product acceptance. Tools, raw logs, line ancestry, independent rebuilds and
manifests receive a new ledger without overwriting history or frozen inputs or
changing packaging/privacy rules.

单 outcome 构造收益扣除了增大的外层节点；多 outcome 仍可能增加内存，最终
报告须列出所有规模的正、零、负收益和时间波动。完整事件、索引、引用、展示数据
及 capture 生命周期债务保留；没有整体恒定内存、任意 OOM 恢复、跨环境改善或
全部源码合理性的证明。本轮未发布、未推送。

Singleton construction benefit includes enlarged outer nodes; multi-outcome
inputs can cost more. Final reporting must retain all-scale positive, zero and
negative results and timing variability. Full-event/index/reference/display and
capture lifetime debt remain. No constant total memory, arbitrary OOM recovery,
cross-host improvement or all-source correctness follows. No publication or push
occurs in this cycle.
