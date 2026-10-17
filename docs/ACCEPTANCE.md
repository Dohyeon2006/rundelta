> 本地证据路径说明：本文及本索引所链接历史文档中的 `target/`、本机实验目录和未发布原始日志路径，仅用于说明原本地复现实验位置；这些产物不随公开版提供，不是可访问的公开证据链接。公开可核验材料以仓库实际收录的文件和公开派生合同为准，不能据本地路径推断已发布原始记录。
> Local evidence paths: `target/`, local experiment directories and unpublished logs describe original local reproductions. Their artifacts are not shipped with this public edition and are not accessible public evidence links.

> 公开树说明（2026-10-16）：本树引用的 ACCEPTANCE-before-reliability、FIELD_CASES、HOLDOUT_RESULTS、RANKING_REPAIR 及对应结果 JSON 为脱敏派生副本；原始证据保留在私有归档，未重新实测。下文关于私有树、历史路径或发布阻塞的结论属于当时状态，不代表本公开副本仍含这些路径。公开派生合同见 [REGRESSION_SET_PUBLIC.json](REGRESSION_SET_PUBLIC.json)；原 v1 原件校验仍属私有树。
> Public references point to redacted historical derivatives, not fresh measurements. Original v1 evidence verification remains private; the public archive has a separate integrity contract.

# RunDelta 可靠性修复验收

最新 CWD 验证见[本页新增章节](#cwd-20260920)。2026-09-20 存储隔离与 CLI 健壮性验收见[本页新增章节](#storage-cli-20260920)。下方 2026-09-19 内容保留为历史验收。

日期：2026-09-19。范围限定为[上次验收的三个发现](ACCEPTANCE-before-reliability.md)、相关 diff 可靠性语义和回归测试，没有扩展产品功能范围。

**本次三个发现均修复，专项审计 FINDINGS: 0。** 13 个单元测试、常规真实案例验收及 8 组可靠性回归通过。仅代表本文列出的场景，不代表白皮书所有阶段或所有 Linux 环境均完成。

## 1. 回归先行与实际结果

先保存修复前可执行文件并新增 tests/reliability.py 的四组回归，再开始修复；它们在旧程序上实际失败：4 tests，7 failures、2 errors。errors 是旧损坏记录 diff --json 没有结构化 JSON 输出导致的断言读取失败，不是被忽略的结果。信号退出日志与实时编号的两个单元回归也先运行失败，再修复。

修复前回归日志（原本地路径 `../target/reliability-before/regressions.txt`）、信号单元红灯（原本地路径 `../target/reliability-before/signal-unit-before.txt`）、[原审计报告（脱敏副本）](ACCEPTANCE-before-reliability.md)均保留。随后增加元数据/标记损坏、写入失败、PTY 和更细的 CLI 语义测试，没有删除旧检测项。修复过程另加红灯回归，确保存储未经验证的提示不计入采集警告，不制造行为差异；行为相同的 v2/v3 比较 differences=false，但仍因 storage=unverified 返回 3。

| 场景 | 修复前实测 | 修复后实测 |
|---|---|---|
| 完整 JSON 行截短 | show 成功；diff 0、无警告、仍宣称完整 | show 2；diff 2，storage=corrupt，differences=null |
| 半行截短、事件文件缺失 | 旧 show/diff 报错，但 JSON 错误结构缺失 | 文本/JSON 均拒绝比较，返回 2，明确文件损坏 |
| 同字节数内容修改 | diff 自比较 0，未识别修改 | SHA-256 不符，返回 2；字节数相等不能绕过检查 |
| 事件数、字节数、marker JSON、metadata 被修改 | 未有完成标记机制 | 所有变体拒绝，返回 2 |
| events 写入失败 | 无可验证的最终完成机制 | 不生成 finalized.json；读取为 unfinalized，diff 3 |
| 目标 SIGRTMIN（本机 34） | record 125，capture_failed | record 162，completed，目标 signal=34 |
| 目标 SIGRTMAX（本机 64） | record 125，capture_failed | record 192，completed，目标 signal=64 |
| 目标正常 return 143/162/192 | 普通 143 已可记录 | 分别 exit_code=143/162/192，signal=null，与信号终止分开 |
| SIGKILL strace | capture_failed | 保持 capture_failed/incomplete，目标状态未知，collector_signal=9，record 125 |
| SIGKILL recorder | 锁释放，但 strace/目标继续存活 | 锁释放，受测 strace/目标/子进程不再存活；记录无最终标记 |
| 不完整记录自比较 | diff 0，仅底部 warning | diff 3，顶部三维状态与显著可靠性提示；JSON 同样表达 |

最终日志目录（原本地路径 `../target/reliability-final/`）含 fmt.txt、unit.txt、integration.txt、audit.txt、clippy.txt。

本轮实际结果：

```text
cargo fmt --check: exit 0
cargo test: 13 passed; 2 integration tests ignored by default
cargo test --test acceptance -- --ignored: 2 passed
  real_capture: 原三个案例和记录管理等验收通过
  reliability_regressions: Ran 8 tests ... OK
cargo clippy --all-targets -- -D warnings: exit 0
python3 tests/audit.py: exit 0
FINDINGS: 0
```

## 2. 可复现命令与真实证据

环境：Rust/Cargo 1.98.1，Linux 7.2.5 x86_64，本地 strace 6.16。Linux 5.3+ pidfd 系统调用需可用；测试另需 cc、Python 3 和 PTY。系统级配置未修改。

```bash
cargo build
export RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace"
cargo fmt --check
cargo test
cargo test --test acceptance -- --ignored
python3 tests/audit.py
cargo clippy --all-targets -- -D warnings
```

可靠性测试也可独立运行 `python3 tests/reliability.py`。每次创建独立 target 子目录，日志打印具体位置。本次：

- 原核心案例（原本地路径 `../target/acceptance-2bxop6xy/`）：执行路径、配置路径、文件消失、重复正常运行及原集成检查。
- 可靠性回归（原本地路径 `../target/reliability-v3z1zdif/`）：commands.json 包含逐条命令、返回码、完整 stdout/stderr；tty-input.txt、tty-interrupt.txt 保存 PTY 输出。
- 专项审计（原本地路径 `../target/audit-9utnrf4z/`）：commands.json、results.json，以及原始运行记录和 diff 文本。

例如：

```bash
target/debug/rundelta --storage target/audit-9utnrf4z/records show target-realtime --json
target/debug/rundelta --storage target/audit-9utnrf4z/records diff collector-killed collector-killed --json
target/debug/rundelta --storage target/audit-9utnrf4z/records diff recorder-killed recorder-killed
```

实际专项输出摘录：

```text
realtime-signal: returncode=162, state=completed, signal=34, collector_signal=34
recorder-SIGKILL: recorder_rc=-9, new_record_rc=0,
  orphan_tracer_alive=false, orphan_target_alive=false
post-save-truncated-events: show_passes=false, diff_rc=2, diff_storage=corrupt
FINDINGS: 0
```

`-9` 是 Python 对 OS 杀死进程的表示。最后一项测试恢复了修改过的文件，不能直接用恢复后的记录重现损坏；应运行回归脚本重新注入。所有 target 证据被 Git 忽略，cargo clean 会删除。

## 3. 存储格式和兼容性

新 metadata schema=3，新增 target_exit_raw；事件 schema 仍为 2（未增加事件字段），读事件支持 v1/v2。diff JSON schema=3。finalized.json 独立 schema=1，包含：

```text
schema_version, run_id, event_count,
events:   { bytes, sha256 },
metadata: { bytes, sha256 }
```

完成协议：

1. 写 events.jsonl.tmp、sync_all、rename、同步目录。
2. 读回事件文件，检查与预期字节一致、可解析且数量匹配。
3. 原子替换最终 metadata.json 并读回校对。
4. 最后写 finalized.json.tmp、同步、rename 为 finalized.json。

写入失败会返回工具错误，不能先发布最终标记。即使 metadata 已更新而最后标记没写成功，也按 unfinalized 处理，不信任 metadata 的 completed。写入失败回归通过预先创建 events.jsonl.tmp 目录制造真实 I/O 错误，不填满用户磁盘、不改系统权限。

list/show/diff 读取新记录都核对 SHA-256、字节数、事件数、标记 ID 和 metadata 状态；用于解析和比较的是验证过的字节。标记只表示数据已最终保存，不表示采集成功：capture_failed 记录也可以 storage=verified。

历史 v1/v2 无校验信息时为 **unverified（存储完整性未经验证）**，不是 corrupt，也不自动补发校验标记。真实历史 v1、显式 v1/v2 格式 fixture 均读取成功；新 v3 为 verified。原始历史数据没有被改写。新 v3 缺少标记为 unfinalized。若旧格式目录实际带标记，仍验证该标记，不通过改小 schema 绕过已有校验。

摘要覆盖 metadata/events，不覆盖 raw 日志；也不是数字签名或 MAC。能同时改写文件和标记的攻击者可以重算摘要，因此**不宣称防恶意篡改**。fsync/rename 不等于已验证断电安全，**没有断电持久性保证**，也未做文件系统故障注入。

## 4. 退出模型与实时信号

目标状态使用 exit_code/signal；strace 状态使用 collector_exit_code/collector_signal，按状态类型校对，不把退出码 143 与 SIGTERM 视为同一种结束。target_exit_raw 留存根进程退出原文。

本机 strace 源码 signal.c 与内核头文件确认 SIGRT_N 以 kernel RT base=32 编号，而 glibc 的 SIGRTMIN() 在本机为 34。因此 SIGRT_2→34、SIGRT_32→64，并检查范围。普通 SIGTERM/SIGINT/SIGKILL、实时最小/最大信号、正常 return 128+signal，以及 strace 被 SIGKILL 都有真实测试。

不认识的结束信号保留为退出事件和原文，数值保持未知，添加警告并使采集 partial；不会仅因信号名称未知就认定采集器失败。此时 record 的返回值依据可观察的采集器状态，diff 返回 3。未知名称/数字及分类保守性目前是固定日志和单元测试证据；不是内核实际发出了“未知信号”的集成测试。

## 5. 生命周期方案与实测边界

原结构为 RunDelta→strace→目标及子进程。SIGKILL 记录器后，原 strace 会被重新托管并继续运行；kill-on-exit 只在 strace 死亡时生效。

实施前评估：

- 额外 supervisor/watchdog 可增加独立收尾与回收能力，但需要新的状态通信和监督进程生命周期，可能影响退出传播与终端处理；本轮不引入。
- 进程组广播既不能保证覆盖 setsid，也可能误杀共享组的无关进程；不采用。
- 采用内核 PR_SET_PDEATHSIG 与已有 strace kill-on-exit：保留原进程结构，改动最小。pre_exec 只调用必要系统调用；设置 SIGKILL 后再核对 getppid，处理父进程先退出的竞态。

正常退出：等待 strace、保存记录、返回目标退出语义。SIGTERM/SIGINT/SIGHUP：经 pidfd 转发给特定 strace，保存可用记录并标 interrupted。SIGKILL：不捕获，不依赖用户态清理；内核通知 strace，ptrace EXITKILL 终止仍在追踪的进程。记录器无法写最后标记，读取为 running/pending + unfinalized，diff 3。

信号转发使用 pidfd，即使 child.wait 后 PID 被重用，旧 fd 也不会指向无关进程；不调用 kill(-pgid)。pidfd 不可用时在启动目标前拒绝运行。获取实际子进程 pidfd 的异常回退只清理仍属于本进程、尚未 wait 的直接 child；其 PID 尚不能重用。

实测包含：正常目标/正常 fork 子进程退出；recorder SIGTERM 与 SIGKILL；单目标、fork 子进程、fork 后 setsid 的受追踪子进程均被清理。测试给内核最多 3 秒调度窗口后确认不再活跃，修复前相同窗口仍有孤儿，修复后均通过。锁异常释放与并发冲突测试仍保留。

没有更改 session/pgroup、stdin/out/err；PTY 输入回显与 Ctrl-C 测试通过。没有测试 shell 的全部作业控制、后台读终端、SIGTSTP/恢复等。

边界：PDEATHSIG 绑定创建子进程的线程，当前由持续存活的主线程创建。要求直接执行可信的非特权 strace；fork 包装器、strace 的凭据变化、setuid/setgid/capability 执行可能清除该设置。主动解除 ptrace、特殊权限/命名空间、双重 fork 守护化未做全面测试。setsid 只有在仍受追踪时才在保证范围内。不可中断内核等待可能延迟退出，不能承诺即时终止；僵尸等待系统父进程回收不等于仍在执行。锁释放与内核清理不是原子步骤。

机制依据：[Linux PR_SET_PDEATHSIG 文档](https://man7.org/linux/man-pages/man2/PR_SET_PDEATHSIG.2const.html)、[pidfd_send_signal 文档](https://man7.org/linux/man-pages/man2/pidfd_send_signal.2.html)、[ptrace EXITKILL 文档](https://man7.org/linux/man-pages/man2/ptrace.2.html)，以及本机 strace --help。设计依据与上面的实际测试结论分开。

## 6. diff 结果与退出码

| 情况 | observed_differences | capture | storage | CLI |
|---|---|---|---|---:|
| 完整、已验证、无差异 | false | complete | verified | 0 |
| 完整、已验证、有差异 | true | complete | verified | 1 |
| 采集不完整（无论有无差异） | true/false | incomplete | 可为 verified | 3 |
| 历史无校验信息 | true/false | complete/unknown 等 | unverified | 3 |
| 新记录无最终标记 | true/false | unknown | unfinalized | 3 |
| 损坏记录 | null | unknown（坏侧） | corrupt | 2 |
| 缺少记录/版本不支持等 | null | unknown（不可读侧） | unavailable | 2 |

状态分别为左右两侧输出；任何一侧可靠性不足就不能返回 0/1。错误优先于可靠性不足。损坏时 changes 为空但 differences=null，不能把空数组读成“无差异”。文本在顶部输出同样字段、退出码与错误；JSON 有 errors 数组。

本次不完整记录自比较的实际 JSON 字段摘录：

```json
{
  "observed_differences": false,
  "reliability": {
    "left": {"capture": "incomplete", "storage": "verified"},
    "right": {"capture": "incomplete", "storage": "verified"}
  },
  "exit_code": 3
}
```

文本同时出现 `RELIABILITY NOT ESTABLISHED`，不只靠 warning。show/list 用于检查记录，读入成功仍返回 0 并展示存储状态，损坏返回 2；它们不表达“两次运行可靠相同”。

## 7. 审计没有降低要求

原审计项全部保留。必要更新均对应更严格的新契约：不完整 diff 的期望 0/1 改为 3，并检查三维状态；截短从“检查有没有警告”加强为 show 拒绝、diff 2、storage=corrupt、differences=null；实时信号增加确切数值/返回码断言。v2 fixture 显式移除不属于 v2 的完成标记，再验证 unverified，而非将 v3 损坏伪装为旧记录。清理观察使用有界等待，但同样窗口已在修复前回归中失败；没有跳过孤儿断言。审计自己也改用 pidfd 清理，避免测试清理向已复用 PID 发信号。

新增 sha2 依赖仅用于 SHA-256，无新后端、数据库或用户功能。此次没有暂存、提交或发布。仓库仍无 HEAD，产品文件未跟踪，普通 git diff 不能代表改动规模；用户原有 .idea/.gitignore 暂存项保留。

## 8. 仍未执行或未覆盖

未做：断电/NFS/磁盘控制器持久性验证、实际 PID 复用压力故障注入、旧内核或禁用 pidfd 的环境集成、完整 shell 作业控制、setuid/capabilities 变化、主动脱离 ptrace、双 fork 守护进程和复杂 PID namespace/共享 FS。PDEATHSIG 设置前父线程死亡竞态有代码保护，但没有逐指令故障注入穷举。

记录定位仍扫描整个存储根目录，其他记录的损坏 metadata 可能阻断当前操作。

白皮书剩余项不变：跨运行精准进程匹配、所有系统调用与上下文语义、raw 日志校验、崩溃记录自动恢复、耗时/日志大小基准、五个真实项目试用均未交付或未验收。原始白皮书逐项表保存在修复前报告；本轮只将三个可靠性发现更新为已修复、已测。

## 9. 后续真实流程评估补充

已实测五个真实工具的受控故障流程，共 15 份记录；这不等于五个外部项目独立试用。[完整报告（脱敏副本）](FIELD_CASES.md) 保存每例前五条评价、复现命令、正常重复对照及证据。前五条直接线索命中 3/5；存储全部 verified，但四组故障比较至少一侧采集 incomplete，diff 均返回 3。

新增发现尚未修复：strace resumed 的对齐空格导致事件遗漏，Cargo 正常对照出现一条已确认的伪执行差异；失败 exec 已被采集却被比较层过滤。GCC 缺头文件的关键变化被排到第 705 条，Cargo 正常对照有 1600 条变化。此前可靠性专项通过仅覆盖其列出的检测项，不能说明这些真实流程没有解析问题。本轮不修改产品实现，下一方向为先修正事实遗漏，再改排序降噪；不优先扩展 CWD 或精准进程匹配。

## 10. 解析与排序修复后的验收

第 9 节为修复前历史。现已修复 resumed 对齐空格遗漏与失败 exec 比较遗漏，并实现排序/透明文本折叠，详见 [本轮验收](RANKING_REPAIR.md)。五个新采集案例全部在第 2 条出现直接线索，原 Cargo 伪执行差异消失；全部存储 verified，前四例采集完整，Cargo 仍因共享 FS 边界 incomplete（diff 3）。原始差异未被折叠删除，Cargo 正常重复仍有 1598 条。

五项回归先失败后通过，当前单元 23 通过、显式平台集成 2 通过（含可靠性脚本 8 项），原专项审计 FINDINGS: 0，新增五例审计通过，fmt/clippy 通过。没有减少原审计项。diff JSON 升为 v4，增加 detail_group 和执行尝试类别；存储格式及可靠性三维语义不变，capture 生命周期代码未改。旧解析遗漏不会通过读取自动修复，历史记录仍保持原始完整性状态。更多独立案例与复杂共享 FS 仍未验收。

## 11. 冻结回归集与新五例

原五例冻结为 REGRESSION_SET v1，不再用于调排序；新增五例在 HOLDOUT_PLAN.md 事先登记预期后真实采集，共 15 份记录。前五线索命中 4/5；定位能力按分析者判断为 2 个可定位、2 个部分线索、1 个不能，不声称用户试用成功率。所有正常对照 0 差异，新例均采集完整且存储 verified，故障 diff 1。checksum-content 未命中按计划保留，排序和 src 未改。见 [新五例报告](HOLDOUT_EVALUATION.md)。

Cargo 共享 FS 的不完整采集与 diff 3 为正确契约，现有 [正式文档示例](INCOMPLETE_CAPTURE.md) 展示三维状态与继续浏览观察差异的方法；不为变绿关闭检测。

<a id="storage-cli-20260920"></a>
## 2026-09-20：存储隔离与 CLI 健壮性修复

范围为架构审查 A1 与 A5 的 stdout 部分。先阅读审查，再新增 tests/storage_cli.py 并在原有二进制上运行，确认红灯后才修改产品。未修改 capture、parse、diff、排序、噪声折叠实验、冻结案例或已有记录格式；测试只创建并变更各自的新目录，没有改写历史记录。未暂存、提交或推送。

### 修复前后实测

初始六组回归：`Ran 6 tests ... FAILED (failures=14, errors=1)`。其中六种 show/list/diff 文本/JSON 输出关闭管道场景都返回 101；两种 /dev/full 输出失败也返回 101。缺失、损坏、不可读 metadata 阻塞健康项；校验失败令 list 整体报错。缺 marker 子项的 error 是旧 list 没有新契约的 directory 字段触发 KeyError，单独列出，不冒充采集失败或跳过。

修复后保留原断言，并增加跨状态 BrokenPipe、索引提示、未来版本、ID 不符、缺字段、symlink/FIFO 等覆盖，共八组通过。

| 场景 | 修复前实际结果 | 修复后实际结果 |
|---|---|---|
| show/list/diff 文本与 JSON，下游关闭 stdout | 101，Rust panic | 正常场景 0，无 panic |
| show 文本/JSON 输出到 /dev/full | 101，Rust panic | 2，stdout output I/O failure，无 corrupt 归类 |
| 存在缺失/坏 JSON/不可读 metadata，读取健康记录 | 2，被无关记录阻塞 | show 0、健康自比较 diff 0 |
| 存在校验失败记录，list --json | 2，整体失败 | 0，坏行 corrupt，健康项仍列出 |
| 缺 finalized 的 v3 | 原可靠性策略已正确 | 保持 unfinalized，show 0、diff 3；list 增加目录定位 |
| 删除 metadata 不可解析的记录 | 原 resolve 依赖解析，失败 | 按明确安全 ID 删除成功；其他项不变 |
| v1/v2 位于坏 metadata 项旁边 | 整体扫描失败 | show 0/unverified，diff 3 |

另外实际执行带 `bash -o pipefail` 的 `show healthy --json | head -n 0` 与文本版本，均 rc=0、stderr 为空。已实测 closed-pipe incomplete diff 仍为 3、corrupt diff 仍为 2；单元测试补齐 0/1/2/3 在 write/flush BrokenPipe 下全部保留。对正常目标 SIGPIPE 的行为未改动。

### 最小恢复边界

store 将目录/标签索引与逐条数据验证分开，capture 原代码仍调用 store::all，但其结果仅为 ID/名称保留信息，不要求健康 metadata。损坏项未知标签不冒充任意名称；可读 JSON 中的名称只是未经验证的索引提示，查询后仍完整校验。标签无法恢复时只能使用 ID；存在多项 ID/名称匹配即拒绝，不优先猜测 ID 或某一名称。

有效内部 ID 使用当前及历史生成的十六进制时间戳与正十进制 PID 形状。list 对普通但残缺目录给出稳定 ID、相对 directory、storage 状态与详情；不安全目录名/非目录/symlink 给出 id=null、unavailable 并保留转义目录定位。删除可恢复的损坏记录不读事件、不依赖 metadata 成功解析，仍持有原 flock；不会自动删除、修复、重写、chmod 或提权。

一般残缺项不阻止新的 record；已知标签与所有有效 ID 仍保留冲突检查。失去的标签无法被预留，这不是历史全局唯一性证明；外部恢复导致歧义时拒绝解析。安全 ID 缺失等不可识别条目存在时 record 保守拒绝。健康项的 show/diff/delete 仍不因此失效；根目录整体无法枚举、操作系统删除权限不足等全局/实际权限错误仍明确失败。

读取 metadata/events/finalized 仅接受普通文件并拒绝符号链接，以非阻塞方式打开避免 FIFO 卡住。没有扩大到恶意并发替换祖先目录的防护保证；读取与删除之间仍可能因外部文件变动失败，不能声称事务快照。

### 输出与兼容性

stdout 写入集中为带 Result 的 write_all/flush。BrokenPipe 只在这一输出边界处理，保留已计算的命令返回码；其他 I/O 错误保留来源和 stdout 上下文并返回 2，不捕捉或吞掉所有工具错误。report 返回渲染文本，排序/折叠/报告事实不变。

持久化 metadata v1/v2/v3、event v1/v2、finalized v1、diff JSON v4 保持。历史没有校验的记录仍 unverified，新格式无完成标记仍 unfinalized，校验失败仍 corrupt。list JSON 的健康行保留原平铺 metadata 字段，新增 directory；损坏行只有可用的 id/name 提示、directory 和 storage_integrity，未知值为 null。list 返回 0 仅表示清单输出完成，消费者必须检查每一行状态，不能推断全部健康。list 顺序按目录名稳定排列。

### 验证命令及结果

```bash
cargo build
cargo test
cargo test --test acceptance -- --ignored
python3 -B tests/audit.py
cargo fmt --check
cargo clippy --all-targets -- -D warnings
python3 -B tests/check_regression_freeze.py
python3 -B docs/external-cases/pybind11-5626/verify.py
```

- 单元测试：26 passed。普通 cargo test 的 3 个平台入口为 ignored；随后显式执行，3 passed。
- 平台入口分别执行常规真实案例、原 8 组 reliability 回归、新 8 组 storage_cli 回归，全部通过。目标信号、实时信号、正常 128+signal、PTY、锁、SIGKILL、旧格式、部分存储失败等原断言未删除或降低。
- 原专项审计：`FINDINGS: 0`；没有移除检测项以使其通过。
- fmt、Clippy `-D warnings` 通过；固定回归与 pybind11 冻结证据校验通过。
- 新隔离矩阵对可读取的损坏项在读取/比较/新建其他记录之后核对内容摘要不变，随后才按明确命令删除。权限拒绝项验证操作结果，未为摘要读取临时恢复权限。

修复前日志：before.txt（原本地路径 `../target/storage-cli-verification-4u5b858c/before.txt`）。修复后：平台集成（原本地路径 `../target/storage-cli-verification-4u5b858c/platform.txt`）、专项审计（原本地路径 `../target/storage-cli-verification-4u5b858c/audit.txt`）、单元测试（原本地路径 `../target/storage-cli-verification-4u5b858c/unit.txt`）。这些是本机被忽略的测试产物，不加入 Git。完整新测试命令转录见 target/storage-cli-qwirb3q_/commands.json；真实采集与注入数据留在各测试独立 target 子目录。

### 仍保留的边界

未测试断电、NFS、恶意同用户并发改写祖先目录、全部文件系统短写/fsync/rename 时点。无 root 下的权限用例已执行；不承诺绕过操作系统权限删除。capture stderr 输出失败、CWD 身份失效、平方级完成扫描、完整模型校验等审查债务未在本轮修复。未新增后端、排名规则或默认折叠规则。

推荐下一项：先为 A2 的目录重命名/CWD 失效写独立回归并验证路径确定性；A3 的单遍完成集合计算可随后作为独立性能修复。本轮不继续扩展。

<a id="cwd-20260920"></a>
## 2026-09-20：CWD 路径不确定性验证与修复

先只读审查 parse::path/resolve_context、Event、diff aggregate 与 show 输出。确认旧实现将初始 CWD 和成功 chdir/fchdir 的历史字符串用于拼接当前相对访问，忽略成功 open 的返回 fd 路径注释。未假设 inode 或事后 realpath 能重建运行时路径。

### 先失败的同步回归

新增 `tests/fixtures/cwd_probe.c` 与 `tests/cwd_uncertainty.py`，在修改解析前运行，**4 tests / 4 failures**。正常、外部重命名、失败 chdir、失败 fchdir 全部实际执行；失败是旧版把无当次路径注释的 access 当成已知绝对路径。

同步顺序为：目标成功 chdir→写 stdout `CWD_READY`→阻塞读 stdin；未被 strace 追踪的 Python 协调器读到 ready→执行并完成 old.rename(new)→写 `G`；目标收到 G 后才执行 SYS_open 和 SYS_access。不是通过 sleep 猜测时序。每例都断言两个调用成功、目标退出 0；在失败切换例中，目标先断言 chdir("missing")/fchdir(-1) 返回失败。异常进程有超时与清理路径。

重命名案例实际事件摘录（`${CASE}` 为脱敏目录占位符）：

| 版本 | 操作 | path | context_uncertain | evidence | 采集完整性 |
|---|---|---|---|---|---|
| 修复前 | open | `${CASE}/old/file` | false | raw/trace.log:6 | complete_for_supported_events |
| 修复前 | access | `${CASE}/old/file` | false | raw/trace.log:7 | complete_for_supported_events |
| 修复后 | open | `${CASE}/new/file` | false | raw/trace.log:6 | partial |
| 修复后 | access | `relative:file [cwd unresolved]` | true | raw/trace.log:7 | partial |

这证明原问题曾真实发生，不只是架构推断。修复后四例均通过：正常对照的 open 保留 old/file 的当次 fd 注释；重命名例的 open 使用 new/file 的当次注释；所有无注释 access 保留 relative:file 与不确定性。cwd_before 均保留最近观察到的 old，失败切换不污染上下文。所有例的自比较 text/JSON 均返回 3、observed_differences=false、capture=incomplete、storage=verified，不把未观察到差异解释成完整一致。

### 实现范围与语义

1. 不再使用历史 CWD 拼接 file/exec 的相对访问路径。
2. 原已支持的完整 dirfd 注释可给出路径表达；新增对成功 open/openat/openat2 返回 fd 的完整 `-xx` 绝对路径注释解码，仅用于尚未解析的相对访问。缺注释、截断注释或 `(deleted)` 注释保留相对/未知，不猜测新目录。
3. cwd_before 仅是历史上下文；成功相对 chdir 的操作链可作词法上下文描述，绝不再作为解析访问路径的依据。fork 继承这一上下文，失败 chdir/fchdir 保持它。当前上下文若涉及共享 FS/namespace，继续原保守警告与未知策略。
4. 相对文件/exec 路径的 context_uncertain=true 并产生警告，已有 capture 完整性逻辑自然降级，不改 capture。cwd 事件的相对操作数本身只表示观察到的切换指令，不因为它是相对参数就单独把访问结论降级；无法解析的 fchdir 仍有警告。
5. show 文本区分记录的绝对拼写、历史 CWD、相对不确定及 path=null；JSON 使用同一 Event 字段。diff 的相对 subject、可靠性和退出码在 text/JSON 中一致。保留所有 evidence 行号，包括 unfinished/resumed 两端。

绝对 syscall 参数和 strace 注释是可检查的追踪观察，不等于文件系统物理身份保证，也不保证注释读取与 syscall 的瞬时原子一致性。不根据返回路径反向修改 cwd。外部进程未被追踪时，**RunDelta 无法证明历史目录重命名事件**；rename 的已知事实来自本测试协调器，而非采集器报告。

### 兼容性与测试结果

未改变 metadata/event/finalized/diff schema，未新增持久化字段；复用 path、cwd_before、context_uncertain 和 warnings。没有重新解析、覆盖旧记录或更改旧校验信息。历史绝对 path 可能仍含旧回退推导，不能因旧标记为 false 就声称安全；show 明确提醒检查历史 raw。旧版阅读器能读取新记录，但不会提供新的解释文字，推荐用本版检查。

三个原单元测试曾期望凭静态 CWD 生成绝对路径，其断言已按新语义替换为**明确的相对路径、精确警告数、不确定标记和继承上下文**；保留父子隔离、失败切换、spawn 关系及证据断言。新增五个单元边界覆盖 fd 注释、缺失/截断/deleted、失败切换、绝对参数与 dirfd、resumed 首尾证据。没有修改专项审计断言或冻结案例。

```bash
cargo build
python3 -B tests/cwd_uncertainty.py
cargo test
cargo test --test acceptance -- --ignored
python3 -B tests/audit.py
cargo fmt --check
cargo clippy --all-targets -- -D warnings
python3 -B tests/check_regression_freeze.py
python3 -B docs/external-cases/pybind11-5626/verify.py
```

实际结果：**31 单元测试通过；4 个平台入口通过**（常规真实案例、原 reliability 8 组、storage_cli 8 组、新 CWD 4 组）；专项审计 **FINDINGS: 0**；fmt、Clippy、冻结回归与 pybind11 证据校验通过。旧专项父子 CWD 测试仍得到正确绝对 open 路径且 complete，来源是当次 fd 注释，没有降低其断言来变绿。

本机日志（被忽略的 target，不加入 Git）：修复前（原本地路径 `../target/cwd-verification-irjvz_ry/before.txt`）、修复后四例（原本地路径 `../target/cwd-verification-irjvz_ry/after.txt`）、单元测试（原本地路径 `../target/cwd-verification-irjvz_ry/unit.txt`）、平台集成（原本地路径 `../target/cwd-verification-irjvz_ry/platform.txt`）、专项审计（原本地路径 `../target/cwd-verification-irjvz_ry/audit.txt`）。最新集成 CWD 原始记录位于 target/cwd-uncertainty-fbfztq89，所有记录及 raw 均保留。

### 未验证与保留边界

未做其他 Linux ABI、复杂 namespace、重命名与 strace 注释读取同时竞态、目录删除后的描述符访问、PID 复用压力测试。无注释访问即使正常，也可能使运行标为 partial，这是必要的不确定性，而不是把正常执行误判为目标失败。运行级聚合仍会合并相同相对拼写，不代表它们指向同一对象；不完整语义必须随报告保留。

capture 生命周期、store 写入格式、diff 排序与噪声折叠规则、冻结案例保持不变。本阶段没有暂存、提交或推送。下一项建议独立解决 A3 的平方级完成检查，并保持本次路径回归不变。

## A3 完成检查单遍聚合（2026-09-20）

基线提交 `1c6e0f777e20b208ede8416e97defb370e943532` 已单独保存 13 个性能报告/脚本/脱敏证据文件，开始实施前确认未包含全局 README 或 IDE 文件。本轮不重复创建空提交；原 `.idea/.gitignore` 暂存项及 README 修改保留。

### 等价性与改动边界

新增 `tests/completion_equivalence.py` 先在旧 dev 上 20/20 通过，再实现聚合。优化前后 dev/release 的持久化事件、非易变 metadata、storage 状态、record/diff 退出码和 reliability 全部相同；仅排除 ID、开始/结束时间。包含首个成功 exec 选根、多个候选、多 exec、fork、交错、子进程先/后退出、缺失根/子退出、重复 child、正常/非零/143、SIGTERM/SIGRT_2/未知信号、采集器失败、无根/空序列、最后 exit 和失败 spawn。

新增单元测试以旧谓词对照六事件的 720 种排列及各 7 个前缀（5,040 次），比较选中 exit 的事件身份与精确缺失 PID 顺序。未知原始行只产生解析警告、不产生 Event 的行为保留；新增测试在改产品前按旧行为校正了最初的假设，并断言告警。

`Completion::from_events` 用借用 PID/exit 的有序表，一次 events 遍历加一次 PID 表遍历，最坏从 O((E+S)E) 降为 O((E+S) log P+P)，保留字符串字典序。第一条成功 exec 的 PID、该 PID 最后 exit、仅 child_pid 被提及也要求退出的规则不变。未知信号仍保留 target_exit_raw，completed/partial 与 capture_failed 不混用。没有修改生命周期、信号转发、CWD 模型、存储格式/校验、v1/v2/v3、排序或冻结回归。

### 性能：实际同模式对照

完整九组 E×P（2k/8k/32k × 1/16/128）、每组三次、min/median/max/标准差/内存/环境与命令见 [OPTIMIZATION.md](performance/a3-completion-check/OPTIMIZATION.md)。32k 事件的中位数范围：

| 模式 | 检查优化前 → 后 | 原生 record 优化前 → 后 |
|---|---|---|
| dev | 6.17–6.37 s → 1.58–4.63 ms | 6.47–6.57 s → 0.514–0.564 s |
| release（两端 opt3/debug2） | 898–962 ms → 0.458–0.686 ms | 1.02–1.17 s → 约 114 ms |

dev-before 使用已提交基线；release-before 从相同提交在独立临时目录构建。原 fixture SHA-256 一致，读取/摘要/计数/完整性验证不变。旧计时仅 missing_exits，新计时含根聚合，未缩小范围以获取收益。release 两次源码断点试测因物理帧不匹配被拒绝，失败数据保留；最终对实际 capture::record 中唯一收集调用的 call/返回机器指令设断点，保留全部校验，不修改产品计时代码。

新状态表需要 O(P) 空间；旧全退出样例的缺失集合原本为空，所以不能承诺内存总是降低。32k 时检查 HWM 前后仍约 dev 21 MB、release 19 MB，范围重叠。更多 PID、其他分布、百万事件、冷盘/NFS、不同 ABI 未测；没有把 release 与旧 dev 混算收益。

### 运行命令和结果

```bash
cargo test
RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace" cargo test --test acceptance -- --ignored --test-threads=1
python3 -B tests/audit.py
cargo fmt --check
cargo clippy --all-targets -- -D warnings
python3 -B tests/check_regression_freeze.py
python3 -B docs/external-cases/pybind11-5626/verify.py
python3 -B docs/performance/a3-completion-check/verify.py
python3 -B docs/performance/a3-completion-check/verify_optimization.py
python3 -B docs/noise-folding-evaluation/test_view.py
```

结果：**dev/release 各 32 单元、dev 的 5 平台入口通过**（含 reliability 8、storage_cli 8、CWD 4、completion 20）；原专项 `audit.py` **FINDINGS: 0 / rc0**；fmt、Clippy、冻结材料、性能数值复算通过。独立噪声实验原 52 文件只读审计也通过。测试状态脱敏列表见 `performance/a3-completion-check/optimization/TEST_RESULTS.json`。

**未通过的附加审计没有隐藏或改断言：** `tests/field_report_audit.py` 对早期历史五例记录 rc1（持久化旧解析警告），优化前二进制读取也失败。随后用原 `examples/field_cases.py` 新采集五例/15 条记录，生成器 rc0，关键线索排名 2/2/3/2/2；原报告审计在 pkg-config good 的 A2 相对路径不确定性警告处 rc1（要求 warnings==[]）。前 A3 版对同一新记录输出完全相同失败。未修改 parse、排序、该审计或原记录以变绿。

进一步将 15 份真实 trace 通过明确标识、不执行目标的重放后端送入旧/新 release，capture→store→show 的非易变完整结果 **15/15 相同**。该结果支持无 A3 语义回归，但不能声称附加五案例审计通过。性能材料保存两次无效 probe 与首次 dev pilot，原日志/构建/私有记录仅留本机，未纳入文档目录。

下一步建议单独核对五案例审计与现有 A2 契约：精确断言预期不确定性、证据和 rc3，不能放宽成任意告警均可。未实施 A4 内存或其他功能扩展。本轮源码、测试、验收和性能增补均未暂存、提交或推送。

## 审计契约修正与 A3 收尾（2026-09-20）

**本节更新上一节“附加审计未全绿”的状态：新契约下五案例附加审计及全部收尾检查通过。** 前次失败、无效性能探针和 TEST_RESULTS.json 历史状态保留，不覆盖旧成绩。本轮没有修改任何产品源码；提交包含的 capture 单遍聚合与 Rust 测试来自已完成的 A3 阶段。

### 先复现，再修正

修正前，对原审计运行当前五案例记录：python-venv 通过后，pkg-config good 因唯一预期 CWD 告警触发 `warnings == []`，退出 1。先增加的告警契约回归在抽取自旧审计的原样判定上运行：10 个测试方法，6 个失败断言（含 phase 子测试）；既证明拒绝了正确告警，也证明缺少应有告警会被旧零告警门禁接受。最终扩展为 13 个测试方法，均通过；其中仍执行冻结 v1 判定并断言它会拒绝同一正确告警。

### 明确、有限的审计契约

产品仍只有字符串 warnings，本轮没有添加产品诊断码。仅在 `tests/field_warning_contract.py` 建立审计分类：识别 `path context unresolved;` 或共享 FS 分类前缀，再核对结构化 Event、进程关系和原始行证据。解释性尾句和行号不硬编码；未知分类直接失败。来源必须是 `raw/trace.log` 的有效 evidence 行，对相对 access 核对 PID、原始参数、访问模式、结果、context_uncertain 和与启动目录一致的历史 CWD。它不证明当前物理目录身份。

| 案例 | good / repeat / bad 预期告警数 | 允许的类型、来源与语义 |
|---|---|---|
| python-venv | 0 / 0 / 0 | 未声明告警，仍要求零告警且采集完整 |
| pkg-config | 1 / 1 / 1 | `cwd.relative_access`；根进程直接创建的 RPM 子进程，成功 exec 的 argv 必须为 `rpm --eval %{_target_cpu}-%{_vendor}-%{_target_os}%{?_gnu}`；只允许 `~/.config/rpm/rpmrc` 的 `access(R_OK) → ENOENT` |
| git-identity | 18 / 18 / 4 | 同类相对 access；逐个固定 `.git/config`、空树/本案例固定提交对象、具体 hook、R_OK/F_OK/X_OK、success/ENOENT 及次数；区分根进程与直接维护子进程，维护 argv 必须匹配。契约声明在代码中，不从本次结果学习 |
| gcc-header | 0 / 0 / 0 | 未声明告警，仍要求零告警且采集完整 |
| cargo-linker | 1 / 1 / 1 | `cwd.shared_clone_fs`；一条全局告警必须有根进程树内成功 clone/clone3 的 CLONE_FS 原始证据；所有不确定事件必须是这些来源，不允许借全局告警掩盖其他 CWD 或 namespace 事件 |

每个契约同时要求 storage verified、state completed、精确正常/异常退出码、target/collector 一致且无终止信号。允许预期告警只意味着**审计符合预期**：原 metadata 必须仍为 partial，文本/JSON diff 必须仍显示 capture incomplete、storage verified、退出码 3；不升级为完整比较。

数量、操作、路径、模式、结果、父子来源或类别不符，缺少预期告警，重复/额外告警，额外未覆盖的不确定事件，错误 raw 行，以及采集/存储/生命周期异常，均拒绝。无声明的新案例默认零告警。工具版本或 Git 配置若改变这些明确模式，审计应失败并要求人工审查，不能自动扩大允许集合。这是本环境冻结样例的契约，不是通用构建告警忽略规则。

### 冻结材料如何保留

原 `docs/REGRESSION_SET.json`、样例输入和排名基线逐字不变。受冻结保护的旧审计保存为 `tests/fixtures/field_report_audit_v1.py`，仍用原 v1 哈希验证；当前审计升级为明确的 v2 告警契约。`tests/check_regression_freeze.py` 从旧脚本施加四处可审查的确定性替换，要求当前脚本恰好等于结果：只替换告警门禁、加入预期/实际展示及更严格的 reliability/rc3 断言。其余排名、前五线索、执行差异、证据、分组及文本/JSON 检查保持原样。新辅助脚本/测试哈希另存 `tests/fixtures/field_audit_contract_v2.json`；没有简单覆盖旧审计哈希来让冻结校验变绿。

### 实际收尾结果

重新运行未修改的五案例生成器，产生新的 15 条记录，再审计：

| 案例 | 首条有用线索排名 | 正常→异常 diff 退出码 | 审计 |
|---|---:|---:|---|
| python-venv | 2 | 1 | PASS |
| pkg-config | 2 | 3 | PASS，预期不确定性保留 |
| git-identity | 3 | 3 | PASS，预期不确定性保留 |
| gcc-header | 2 | 1 | PASS |
| cargo-linker | 2 | 3 | PASS，共享 FS 不确定性保留 |

每个 phase 的 stdout 和 repair-audit.json 均包含 expected、actual、原始 warning 字符串及事件证据索引。公开脱敏汇总为 [AUDIT_CLOSURE.json](performance/a3-completion-check/optimization/AUDIT_CLOSURE.json)。原始 records/raw 留本机，不随提交公开。

- 告警契约单元：**13/13**，包括来源、模式、数量、语义及误接受反例。
- 对上述全部 15 条实际记录做只读内存变异：**222/222 必须拒绝的变异均被拒绝**；记录文件摘要前后相同。
- Rust 单元 **32/32**，显式平台集成入口 **5/5**，原专项审计 **FINDINGS: 0**。
- fmt、Clippy、v1/v2 冻结验证、pybind11 冻结证据、噪声实验 8 单元及原材料审计通过。
- A3 基线复算及优化复算通过：原 9 组/27 样本、81 个最终优化对照样本、27 个保留 pilot、两次被拒绝的 probe 均保留。本轮没有重测或重写性能实现/成绩。
- 额外负向控制：早期含真实解析告警的历史记录仍被新审计拒绝（预期退出 1），没有借本次契约放行。

```bash
python3 -B tests/test_field_warning_contract.py
python3 -B examples/field_cases.py
# ARTIFACTS 取生成器打印的新目录，不覆盖既有记录
python3 -B tests/field_report_audit.py "$ARTIFACTS"
python3 -B tests/field_warning_mutations.py "$ARTIFACTS"
cargo test
RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace" cargo test --test acceptance -- --ignored --test-threads=1
python3 -B tests/audit.py
cargo fmt --check
cargo clippy --all-targets -- -D warnings
python3 -B tests/check_regression_freeze.py
python3 -B docs/external-cases/pybind11-5626/verify.py
python3 -B docs/performance/a3-completion-check/verify.py
python3 -B docs/performance/a3-completion-check/verify_optimization.py
```

本轮要求的检查无环境阻塞；A3 未测的其他 ABI、大于 128 PID、百万事件/其他性能分布等边界继续保留。拟独立本地提交 `Optimize completion-check aggregation`，包含 A3 实现/测试、审计契约和验收性能材料；排除所有 README、IDE、构建产物、临时目录、原始追踪和第三方源码。不推送，不改 Git 全局配置。

## CLONE_FS 共享 FS 上下文建模（2026-09-20）

### 范围、旧行为与实现

本节在提交 `c5f02f801d7ab68032dc4704a0f8767b8108b584`（仅 external alpha 证据）之后完成。开始实现前，新增的 11 个解析回归在旧实现上 **11/11 失败**：旧代码只要看见一次 `CLONE_FS` 或 namespace 事件，就把整次记录的 CWD fallback 置空。共享成员的成功 `chdir` 不会传给其他成员，私有分支与较早事件也被无关共享关系清空。

修复后，解析期维护 `Task → FsContext` 和 `FsContext → {最近观察 CWD、不确定状态、成员计数}`。成功 `clone`/`clone3` 带 `CLONE_FS` 的子任务别名父任务的同一上下文；`fork`、`vfork` 和已证明不带该位的 clone 得到私有的观察状态副本。成功 `chdir`/可验证 `fchdir` 只更新所属上下文；`unshare(CLONE_FS)` 分裂调用者；最后一个成员退出时才释放上下文。`execve` 不清空或复制该上下文：这是依据 Linux `copy_fs`/`unshare_fs` 源码和本机真实探针的共同结论。

实现没有把历史 CWD 当成当前物理目录。相对 `access` 仍为 `relative:... [cwd unresolved]`，即使 `cwd_before` 有最近观察值；当次完整 dirfd/返回 fd 注释或绝对操作数才是该事件的路径证据。CWD 重命名后的原有保护保持不变。完整状态机、证据排序和无法证明的边界见 [FsContext 模型](FS_CONTEXT_MODEL.md)。

| 原始证据 / syscall 结果 | FsContext 动作 | 完整性语义 |
|---|---|---|
| `clone`/`clone3` 明确 `CLONE_FS` 成功 | 子任务共享父上下文 | 可继续完整 |
| `fork`、`vfork` 或已解析的不带 `CLONE_FS` clone 成功 | 子任务私有复制父的观察状态 | 可继续完整 |
| clone flags 缺失、缩写或无法解释 | 不猜测共享关系 | 相关上下文 incomplete |
| `chdir`/可解码 `fchdir` 成功 | 更新所属上下文 | 可继续完整 |
| 失败 `chdir`/`fchdir` | 不改变上下文 | 失败 fchdir 的操作数缺失仍保留既有告警 |
| `unshare(CLONE_FS)` 成功 | 调用者获得私有上下文 | 可继续完整 |
| `CLONE_NEWNS`、`setns`、`chroot`、`pivot_root`，或重叠的上下文转换 | 不伪造路径关系 | 受影响上下文 incomplete |
| exec | 保留同一 FsContext | 已以共享/私有 exec 探针验证 |

待完成 syscall 与其他任务的事件交错时，更新会延迟到返回行；无法证明顺序时只使相关上下文不确定，并保留原始开始/结束行号。没有全局清空，也不依赖线程组、父子 PID、工具名或路径字符串猜测共享关系。

### 实际回归和真实采集结果

新增的 Rust 解析回归覆盖：单根、pthread 和跨线程组共享、私有 fork/clone、混合分支、多次 CWD 切换、成功/失败 `unshare`、exec、成员退出顺序、unfinished/resumed 交错、未知 clone flags、CWD 重命名后的相对路径、失败 `chdir`/`fchdir`、namespace 边界和返回 FD 注释优先级。它们在修复后 **11/11 通过**；全部 Rust 单元为 **43/43**。

`tests/fs_context.py` 用本机 C 探针和管道同步，让切换发生在读取方的相对 `open` 前；探针读取 A/B 两个同名文件的实际 marker，而不仅检查 trace 字符串。真实 strace 采集的 3 个测试方法均通过：

| 场景 | 实际读取 | CWD/路径结果 | capture / self-diff |
|---|---|---|---|
| 正常对照 | A | A 的 `cwd_before` 和当次绝对 FD 注释 | complete / 0 |
| pthread `CLONE_FS`、跨线程组 `clone(CLONE_FS)` | B | 另一成员的 `chdir(B)` 传播到读取者 | complete / 0 |
| 私有 fork、混合私有分支 | A | 父或无关共享分支不改变私有读取者 | complete / 0 |
| `unshare(CLONE_FS)` 后 | A | 后续共享成员的切换不传播回来 | complete / 0 |
| shared exec / private exec | B / A | exec 后分别保留共享 / 私有关系 | complete / 0 |
| 失败 `chdir` | A | 不继承失败切换 | complete / 0 |
| 失败匿名 `fchdir` | A | CWD 未变；既有操作数证据告警仍在 | incomplete / 3 |
| 缺失或不可解析 clone flags | 不作路径推断 | `fs.context` 告警 | incomplete / 3 |

最后两行还检查文本和 JSON 同时输出相同的 capture/storage 语义：完整自比较为 0；有 `fs.context` 不确定性的自比较为 3，JSON 的 reliability 为 `capture=incomplete, storage=verified`，文本包含 `RELIABILITY NOT ESTABLISHED`。这不是把无差异误报为可信无差异。

以下命令在本机 Fedora 44 / Linux 7.2.5 x86_64、strace 6.16、Rust 1.98.1 下实际执行：

```bash
cargo test
RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace" \
  python3 -B tests/fs_context.py
RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace" \
  cargo test --test acceptance -- --ignored --test-threads=1
python3 -B tests/audit.py
python3 -B tests/test_field_warning_contract.py
cargo fmt --check
cargo clippy --all-targets -- -D warnings
python3 -B tests/check_regression_freeze.py
python3 -B docs/external-alpha/clone-fs-attribution/verify.py
python3 -B docs/external-cases/pybind11-5626/verify.py
python3 -B docs/performance/a3-completion-check/verify.py
python3 -B docs/performance/a3-completion-check/verify_optimization.py
```

结果：Rust 单元 **43/43**，平台入口 **6/6**（原 5 项加 FsContext 入口），专项审计 `FINDINGS: 0`，告警契约 **13/13**，fmt、Clippy、冻结集、external-alpha 扩展校验、pybind11 脱敏材料校验和 A3 数值校验均返回 0。平台入口包括真实三例、可靠性、存储/CLI、CWD、完成检查和新增 FsContext 探针。

五案例审计还以已有的冻结记录通过。对本轮偶然生成的一组新鲜五案例记录，`field_report_audit.py` 在 Git repeat 例上返回 1：trace 的 unfinished/resumed 调度使原始开始行和返回行从 118/118 变为 117/118，而该历史审计要求两者相等。它不是 FsContext 语义放宽，也没有修改、删除或隐藏该断言；`field_warning_mutations.py` 因同一个先决契约失败。该环境敏感失败保留为未解决的审计可移植性问题，不能称“全部附加审计通过”。

### 格式兼容和旧记录

本轮没有改变记录格式：Event schema 仍为 2，Metadata 为 3，`finalized.json` 为 1，v1/v2/v3 读取策略未变。clone flags 仅在记录当时从 raw trace 进入解析器的临时状态；没有新增持久化 flags 字段，也不重新解析读取中的历史 events。

- v1 没有完整性字段，capture 显示 unknown 且 storage 为 unverified；不会被新版本提升为 complete。
- v2/v3 保留保存时的 metadata、warnings 和 completeness；旧记录不因新模型重新得出共享关系。
- 旧的全局 CLONE_FS 告警记录会继续是 partial/incomplete；新模型不修改既有记录。
- 若历史记录缺少足以证明共享关系的信息，RunDelta 不以读取时的猜测补齐它。v3 新记录仍必须先通过 marker 的存储校验，才能被标为 storage=verified。

因此这种兼容策略保留历史结论，而不是把旧格式伪装为使用了新的 FsContext 证据模型。

### 性能和内存复测

用 A3 的同一九组真实 `record → parse → finish → list → diff → show` 合成路径重测；每组 3 次，输入为 2k/8k/32k events × 1/16/128 PIDs。计时器通过 GDB 在 `Completion::from_events` 调用周围取样，原生 record 和 GNU time 另记整个管线峰值 RSS；没有用手写循环代替产品路径。现有 `measure.py` 的固定源码行断言已经与 A3 后的 capture 行号不符，所以使用其已提交的 `compare.py` 位置适配器；适配器仍要求唯一调用、三断点各一次、record 栈帧、完整存储读取和 self-diff 校验。

32k / 128 PID 的同构建模式结果如下，单位为 ms/KiB，括号为 3 次 min–max：

| 构建 | 版本 | 完成检查中位数 | 全 record 中位数 | 检查末 HWM 中位数 | record 峰值 RSS 中位数 |
|---|---|---:|---:|---:|---:|
| dev | A3 后、FsContext 前 | 4.626 [4.585–5.065] | 564.476 [514.343–564.488] | 21,252 | 51,168 |
| dev | 当前 FsContext | 4.759 [4.534–4.830] | 564.380 [564.334–564.413] | 21,704 | 51,148 |
| release opt3/debug2 | A3 后、FsContext 前 | 0.686 [0.671–0.841] | 113.731 [113.702–113.758] | 19,576 | 49,452 |
| release opt3/debug2 | 当前 FsContext | 0.669 [0.662–0.911] | 113.735 [113.731–113.755] | 19,616 | 49,376 |

当前解析器状态按任务数加 FsContext 数线性增长，且在 parse 结束后释放。完成检查本身未改动，故其小幅波动不能归因于 FsContext；整个 record 的 50 ms 量化下没有可辨认的吞吐回归。dev HWM 的约 452 KiB 差和 release 的约 40 KiB 差也不能从三次样本中宣称为稳定增量或泄漏。未测百万 events、超过 128 PID、其他 ABI、冷盘/NFS、OOM、真实高并发扇出。

```bash
CARGO_PROFILE_RELEASE_DEBUG=2 cargo build --release --locked
python3 -B docs/performance/a3-completion-check/compare.py \
  --source-root . --binary target/debug/rundelta \
  --profile dev-fs-context --output /tmp/fs-context-dev.json
python3 -B docs/performance/a3-completion-check/compare.py \
  --source-root . --binary target/release/rundelta \
  --profile release-fs-context-opt3-debug2 --machine-call \
  --output /tmp/fs-context-release.json
```

输出仅保留在本机临时目录，未加入仓库；公开 A3 基线及其校验材料不被改写。

### SPAdes 和仍未验证的边界

external-alpha 的旧材料没有改动。保存的 SPAdes #1339 摘要仍是 `partial / verified / diff=3`：它除旧版的 CLONE_FS 全局告警外，还含一个未知未完成 syscall。当前工作区没有保存该私有完整 raw trace，公开材料也刻意不包含它。因此没有把一个合成 trace 当作 SPAdes 重跑，也没有声称本修复会使 SPAdes 完整；读取既有记录的历史结论保持不变。

当前可以从 incomplete 变为 complete 的范围是：新记录中 clone flags 明确证明 `CLONE_FS`，所有相关 CWD 转换有完成/可解释的 evidence，并且没有其他 parser/capture/storage 警告。以下仍必须 incomplete 或未验证：未知/缺失 flags、重叠未完成转换、无目录证据的成功 fchdir、namespace/root 改变、相对访问无当次路径证据、记录损坏，以及非 leader 线程 exec 时 strace 的 PID 迁移/`superseded by execve` 表示。

外部未追踪进程的目录 rename、脱离进程组/守护化、复杂 `setns`、目录 FD 关闭或复用、非 UTF-8 raw 全链、其他 Linux ABI，以及 PID/TID 身份迁移都没有被这个模型证明。下一项推荐工作是先以真实 trace 建立非 leader 线程 exec 的 PID 迁移回归；在那之前保持相关上下文 incomplete，而不把它错误地接到旧 FsContext。

本轮产品源码、测试和文档均保持未暂存；没有推送或修改 Git 全局配置。external-alpha 证据已在本轮开始前单独提交，hash 为 `c5f02f801d7ab68032dc4704a0f8767b8108b584`。

## 冻结证据与新鲜采集审计分层（2026-09-20）

本节收尾上阶段 FsContext 的审计阻塞。**没有再修改产品源码、FsContext、解析器、排序、存储格式或 capture 生命周期。** 本阶段开始时的 122 个受保护既有文件（包括源码、冻结审计、外部材料、README 和 IDE）内容摘要全部一致；历史五案例与此前失败的 fresh 五案例各 61 个 records 文件（包含 raw 和锁文件）前后摘要一致。旧记录未重新采集或改写。新审计及其契约在全新五案例采集前登记摘要，采集后核对生成器、二进制、审计、契约和测试字节未变。

### 原失败保留与更正

上一节原样保留了 **`118/118 → 117/118`** 的失败速记和旧审计退出 1 的结论。本阶段回查保存的 events 与 raw，确认坐标应为 **`118/118 → 117/119`**：Git root 对 `.git/hooks/post-commit` 的 `access`，模式 `X_OK`、结果 `ENOENT`；repeat 第 117 行 unfinished，第 119 行才是同 PID 的 resumed，**第 118 行是其他 PID 的 openat**。旧速记的结束坐标错误不应成为审计依据。

保存的该 Git good/repeat events 文件 SHA-256 分别为 `68cd1035c86535d137ec7e3c2f4311f8faa9be505f2cc78138e5b6122d9056ea`、`939889f6635990316d9e8332dc8b3ecacfef53767d179a610b0e836337313767`，本轮未变。再次运行旧 v2 审计仍在 `end_line == warning_line` 断言处退出 **1**；这是保留的预期负向证据，不称为该输入通过旧审计。在内存中将结束坐标误写为 118，新证据核验也明确拒绝；正确的 117/119 则通过。

归因：历史审计的单行约束被错误用于可变调度的新鲜采集，不是该 access 丢失或变成不同 syscall。旧 `field_warning_mutations.py` 因相同前提失败的历史结论保留；本轮对**历史冻结输入**运行它通过。新实现没有删 Git、放宽排名、忽略警告或改写历史哈希。

### 两层实测

设计、精确契约、来源与反例见 [五案例审计分层](FIELD_AUDIT_LAYERS.md)。历史 v1/v2 fixture、manifest、`field_report_audit.py`、`field_warning_contract.py`、原变异测试和冻结校验器逐字不变。独立入口 `fresh_field_audit.py` 检查全部 file/exec 的 raw 语义、完整差异集合、关键线索的进程来源及计数、精确告警、可靠性、退出码和文本/JSON 前五一致性。它不与历史 PID 或行数比较。

| 案例 | 历史冻结审计 | 新鲜正常→异常变化数 | 新鲜正常→正常变化数 | 关键线索排名 | 新鲜 diff 退出码（异常 / 对照） | 新鲜 capture / storage |
|---|---|---:|---:|---:|---|---|
| python-venv | PASS，第 2，diff 1 | 106 | 0 | 2 | 1 / 0 | complete / verified |
| pkg-config | PASS，第 2，diff 3 | 7 | 0 | 2 | 3 / 3 | incomplete / verified |
| git-identity | PASS，第 3，diff 3 | 42 | 8 | 3 | 3 / 3 | incomplete / verified |
| gcc-header | PASS，第 2，diff 1 | 1,255 | 2 | **2（精确要求）** | 1 / 1 | complete / verified |
| cargo-linker | PASS，第 2，diff 3 | 2,054 | 1,614 | **2（精确要求）** | 1 / 1 | complete / verified |

新鲜 15 条记录中，正常目标均返回 0；异常目标/采集器分别返回 1、1、128、1、101。预期/实际告警完全一致：Python/GCC/Cargo 为 0/0/0，pkg-config 为 1/1/1，Git 为 18/18/4。前五和警告之外的观察没有删除；例如 Cargo 正常重复仍有 1,614 条动态构建观察差异。

历史 Cargo 继续保留原全局 CLONE_FS 告警并以 incomplete/diff 3 读取；当前新鲜 Cargo 必须零告警并满足已实现 FsContext 的 complete 条件。新契约遇到旧 CLONE_FS 告警也会失败，绝非将其忽略。SPAdes、外部 alpha 材料和已有记录没有随本结果升级。

本次全新采集有 **132 条** file/exec 事件使用跨行证据，均按实际同 PID、同 syscall 的入口/返回核验。其 Git CWD 告警恰好均在单行；对上一组失败的 15 条实际记录，新审计另行通过，包括那条跨行 post-commit access。再结合合成的稳定插入/分行正例，这些是已执行的证据，而非声称本次恰好复现了所有调度。

### 检查及实际输出

| 检查 | 结果 |
|---|---|
| 历史冻结五案例审计 | **5/5，退出 0**；原始行号和断言保留 |
| 新鲜五案例审计 | **5/5，退出 0**；15 条新记录、10 次比较，保留 Git repeat |
| 上阶段失败记录的新鲜语义审计 | **5/5，退出 0**；旧审计对该输入仍预期退出 1 |
| 原告警契约单元 | **13/13** |
| 原历史记录告警变异 | **222/222 拒绝，15 条记录未改写** |
| 新鲜审计正反例 | **25/25**；错误 syscall、结果、路径/确定性、角色、未知/不完整额外告警、相同行号错误语义全部拒绝；正确重定位/分行通过 |
| Rust 单元 | **43/43** |
| Linux 平台入口 | **6/6 显式运行**：三例、reliability、storage/CLI、CWD、completion equivalence、FsContext |
| 专项审计 | **`FINDINGS: 0`** |
| fmt / Clippy all-targets `-D warnings` | **退出 0 / 0** |
| v1/v2 冻结材料 | **通过，未改 fixture/哈希/断言** |
| external alpha 原范围与扩展 | **通过**，旧固定白名单在原 26/32 文件快照通过，扩展使用独立检查 |
| pybind11 冻结与脱敏 | **通过：446 keys / 712 calls / 420 paths** |
| A3 数值材料复算 | **通过：27 基线样本，81 最终优化样本、27 pilot，原失败 probe 证据保留** |
| 噪声折叠独立单元 | **8/8**，既有实验材料未改 |

旧 alpha 白名单若直接作用于后来扩展的树，仍按原标准返回 1；扩展校验器显示并验证此拒绝，未把它报告成旧白名单支持了新目录。本次所要求的历史/新鲜审计和平台检查没有环境阻塞。开发中新审计一度拒绝设备 FD 属性语法的日志也保留，修正范围和新增反例见分层设计；正式新鲜采集在该审计修正之后执行。

本机为 Linux x86_64 `7.2.5-200.fc44.x86_64`、strace 6.16、Rust 1.98.1、Cargo 1.98.1、Python 3.14.7、Git 2.55.0、GCC 16.2.1。关键实际 stdout：

```text
PASS python-venv: clue=2, control=0, diff=1
PASS pkg-config: clue=2, control=0, diff=3
PASS git-identity: clue=3, control=8, diff=3
PASS gcc-header: clue=2, control=2, diff=1
PASS cargo-linker: clue=2, control=1614, diff=1
PASS: fresh five-case semantic audit; all local evidence, exact warnings, ranking and reliability checked
test result: ok. 43 passed; 0 failed
test result: ok. 6 passed; 0 failed; 0 ignored
FINDINGS: 0
```

```bash
cargo test --locked
RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace" \
  cargo test --locked --test acceptance -- --ignored --test-threads=1
python3 -B tests/audit.py
python3 -B tests/test_field_warning_contract.py
python3 -B tests/test_fresh_field_audit.py
python3 -B tests/field_report_audit.py "$FROZEN"
python3 -B tests/field_warning_mutations.py "$FROZEN"
python3 -B examples/field_cases.py
python3 -B tests/fresh_field_audit.py "$FRESH"
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
python3 -B tests/check_regression_freeze.py
python3 -B docs/external-alpha/clone-fs-attribution/verify.py
python3 -B docs/external-cases/pybind11-5626/verify.py
python3 -B docs/performance/a3-completion-check/verify.py
python3 -B docs/performance/a3-completion-check/verify_optimization.py
python3 -B docs/noise-folding-evaluation/test_view.py
```

`FROZEN` 与 `FRESH` 的输入约定见分层设计；实际原始采集目录只在本机日志中保存，不在公开文档写用户路径。本节的比较数量属于实际测试，不是新的不可变基线；可变临时路径数量不能代替关键线索、角色、告警、退出码等固定契约。

### 边界与提交范围

新审计并不是 FsContext 的第二套实现；任务共享语义仍由已有 43 单元及真实同步探针验证。未测所有调度/其他 ABI、非 leader exec 身份迁移、PID 重用和复杂 namespace；没有新跑 SPAdes 或重做噪声实验私有全量数据，也没有增加其可信等级。性能和内存数字仍引用上阶段测量；本阶段没有性能代码变更或新的速度收益声明。

本地收尾提交 `Model shared filesystem contexts safely` 仅纳入上阶段 FsContext 源码/测试、本阶段独立审计/测试与验收、架构、模型文档。README、A3 README、IDE 原暂存状态和未跟踪文件保留，不纳入提交；不包含 target、raw、第三方源码或临时工作目录，不推送、不修改 Git 全局配置。


## 独立 CWD-tree warning 合同（2026-10-16）

`tests/cwd_tree_warning_contract.py` 与 `tests/test_cwd_tree_warning_contract.py`
定义独立的 CWD-tree 合同；新清单
`tests/fixtures/cwd_tree_warning_contract_v1.json` 固定这两个文件的 SHA-256，
由 `tests/check_cwd_tree_warning_contract.py` 检查并接入 full_gate；正反例接入
`--audit-oracles`。这不扩展历史五案例 v2；旧清单、冻结校验器、分类模块和原变异测试
保持原字节，既有历史结论与正文保留。

该用例必须恰好有两条 `fs.context:` 和一条 `path context unresolved;` 告警。
每条必须引用 `raw/trace.log` 的有效正整数行号：两条 fs.context 分别对应
`chdir("parent") = 0` 和 `chdir("../child") = 0`；path context unresolved
必须对应失败的 `chdir("does-not-exist") = -1`。行号随实际原始证据变化，
但引用必须准确匹配指定 syscall、参数与成败；解释性尾句允许变化。
缺失、重复、未知或错误类别、错误文件/行号，以及错误 syscall 参数或成败
均须失败。audit.py 继续保留三个文件事件、绝对路径及当次 raw 路径注释关联断言。

The independent cwd-tree contract requires exactly two `fs.context:` warnings
and one `path context unresolved;` warning, each citing a valid `raw/trace.log`
line with the specified chdir operand and success/failure. Explanatory suffixes
may change; missing, duplicate, unknown or incorrect categories, incorrect
references and syscall mismatches fail closed. This does not extend historical
five-case v2 or change its frozen files and manifests.
