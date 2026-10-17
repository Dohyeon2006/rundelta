# RunDelta 只读架构审查

> 历史审查基线：本文保留 2026-09-20 各轮发现、当时行号与阶段性修复记录，不是 2026-09-21 全量重构后的当前实现清单。包括 `read_to_string`、diff 深复制、signal relay、固定临时文件和旧模块可见性在内的描述应按其日期阅读；当前产品合同见根 README、[本轮重构决策](REFACTOR_DECISIONS.md) 与源码回归。

更新（2026-09-20，A3）：完成检查单遍聚合已实现并通过等价性及 dev/release 性能验收；详见下方 A3 与 [ACCEPTANCE](ACCEPTANCE.md)。以下早期审查结论按各节更新状态读取。其附加五案例审计的过期契约已在收尾阶段按有限、可验证的 v2 告警声明修正，当前全部收尾检查通过；历史失败记录仍保留。


审查日期：2026-09-20。基线提交：`b6239b786264db1e6fd8ed77d5440e1be8227b52`。

本轮仅审查，不实施重构。提交评估材料之后，仅新增本报告；未修改产品源码、测试、既有文档、暂存区或配置。未运行会生成构建/采集文件的测试套件。优先级按正确性、可恢复性和实际命令规模风险排列，不把风格偏好计入债务。

**结果：未发现 P0；9 项重构债务（3 项 P1、6 项 P2），另列 3 项观察。** 一项问题通过只读运行现有二进制确认；其余明确标为代码审查推断或未验证风险。已有验收文档中的测试通过属于历史证据，不作为本轮重测结论。

## 修复状态更新（2026-09-20）

本节是后续修复阶段的更新，原只读结论和原位置依据基线提交保留在下文，不能把原行号当成修复后位置。

- **A1 已修复（在明确的恢复边界内）**：新增红灯回归确认问题；目录索引与单条验证分离，坏项不阻塞健康项，list 保留 ID/目录/状态，损坏项可按安全 ID 删除。未实施初始 staging 目录；首次 metadata 缺失已能作为单条残缺项隔离。原建议中“未知历史标签必须阻止重名判断”的方向被收窄：为满足新 record 可用性，只预留可恢复标签和有效 ID，未知标签不参与命名，外部恢复后产生歧义则拒绝，绝不声称证明历史标签唯一。
- **A5 已修复 stdout 部分**：文本/JSON 的 BrokenPipe 无 panic，保留命令状态；其他写入/flush 错误有 stdout 诊断并返回 2。capture 内部 stderr 的未验证风险保留，本轮按范围未修改 capture。
- **A4 部分改善，仍保留**：list 逐条验证并释放 Run，不再持有所有记录的完整 events；diff 聚合深复制与 finish 多次物化未改动。
- **A6/A7/A8/A9、A2/A3 保留**：没有实施模型、监督、排序、字节或 CWD/复杂度重构。metadata 读回复用既有校验函数及普通文件限制不等于完成 A6。
- **未推翻已实测的问题；A1 恢复策略按本阶段要求调整。** 没有新增 P0。详细前后结果、格式兼容与测试见 [最新验收](ACCEPTANCE.md#storage-cli-20260920)。26 个单元测试、3 个平台入口（含原 reliability 8 组、新 storage_cli 8 组）、专项审计、fmt 和 Clippy 通过。

下一步建议从 A2 的目录身份变化回归开始；A3 可单独验证性能修复。原 A1 不再是待解决的首项，A5 的残余仅限未改动的 capture stderr。

## CWD 验证更新（2026-09-20，A2）

**A2 从代码推断升级为实测问题，并在本阶段完成保守修复。** 四组明确同步回归在旧版本全部失败；重命名后 open/access 均被写成 old/file、context_uncertain=false、采集 complete。修复后只采用当次可完整解码的 fd/dirfd 注释或绝对参数作路径证据，无证据的相对访问保持相对且 uncertain；cwd_before 只作历史/词法上下文。失败 chdir/fchdir 不替换上下文。

没有通过 inode、事后文件系统或旧字符串猜测新目录。外部协调器未被追踪，RunDelta 无法证明历史 rename；open 的 new/file 只是 raw 中的 fd 注释观察。未知路径使 diff 保留退出码 3，文本/JSON 一致；旧记录不会自动重写，旧绝对拼写仍需查 raw。

[最新验收](ACCEPTANCE.md#cwd-20260920)：31 单元测试、4 平台入口、专项审计、fmt、Clippy 通过；原审计与冻结断言未降低。三个依赖旧 CWD 绝对化假设的单元断言明确改为相对值、上下文与不确定性断言。A2 的错误绝对化路径已修复；物理身份/并发时刻证明、复杂 namespace、历史重命名解释等仍不在保证范围内。

原 A1 与 A5 stdout 修复保留，其余债务不变；下一优先项建议 A3 单次集合完成检查。本次不实施性能或排序变更，也不暂存、提交或推送。

## CLONE_FS 建模更新（2026-09-20）

**A2 中“看到共享 FS 后全局清空 CWD”的缺陷已修复；不是格式重构，也没有新增 P0。** 这项结论来自先在旧实现上失败的 11 个解析回归，以及新 C 探针的实际 strace 采集，不是仅凭代码推断。旧逻辑以一次运行级 `shared` 布尔值禁用每个 PID 的 CWD fallback，导致共享成员之间不能传播已证实的切换，且 mixed/private 分支、早于共享事件的观察也失去上下文。

当前解析期的 `FsState` 将任务与 FsContext 分开，显式维护任务到 context 的关系、context 的成员数、最近观察 CWD、待完成转换和不确定状态。成功 `CLONE_FS` 将 child 连接到 parent 的 context；fork/vfork/已解析的非 FS clone 创建私有观察副本；成功 `unshare(CLONE_FS)` 分裂调用者；exec 保留 context；退出只在最后成员时释放。`BTreeMap` 保证状态输出不依赖哈希迭代，瞬时状态为 O(tasks + contexts)。没有更改 Event v2、Metadata v3、finalized v1 或 v1/v2/v3 的读取策略。

| 审查项 | 当前状态 | 证据 |
|---|---|---|
| CLONE_FS 共享 CWD 传播 | 已修复 | pthread 与跨线程组 clone 探针：一个成员切到 B，另一成员实际读取 B；完整/self-diff 0 |
| 私有 fork 与 mixed 分支隔离 | 已修复 | parent/无关 shared 分支切换后 private 读取者仍读取 A；完整/self-diff 0 |
| `unshare(CLONE_FS)` 与 exec | 已修复于已测基本形态 | unshare 后读取者保持 A；shared exec 读 B、private exec 读 A |
| 未知 flags / 重叠转换 | 有意保持 incomplete | 解析与 synthetic record 回归要求 `fs.context`、JSON/text reliability 和 rc3 |
| CWD rename 的虚假绝对路径 | 既有修复保持 | 相对 access 仍相对且 uncertain；当前 syscall 的完整 FD 注释才保留绝对观察 |

**剩余 P2：非 leader 线程 exec 的任务身份迁移。** 本机 trace 表明这种 exec 可能有 PID 变更或 `superseded by execve` 表示；现有 Task key 没有显式迁移操作。该边界没有被模型猜测，可能转为 `fs.context` 不确定或其他解析 warning，不能当作共享 FS 已完整建模。最小修复方向是先保存真实 trace fixture，定义身份迁移与成员计数的一次性原子操作，并覆盖 clone→nonleader exec→后续相对访问、退出顺序和 PID reuse。**先写回归：需要。**

**保留的 P2 / 观察边界：** 目录身份不是字符串 CWD；未追踪外部 rename、无可解码 fchdir 目录、`setns`/root/mount namespace、未知 clone flags、未完成交错和存储损坏继续降低可信度。旧 records 不重新解析或升级，因此没有历史记录会因这个新模型被标记为完整。相关状态机、Linux 依据和限制见 [FsContext 模型](FS_CONTEXT_MODEL.md) 与 [验收](ACCEPTANCE.md#clone_fs-20260920)。

性能复测没有显示 A3 完成检查回归：同一 32k/128 PID 路径三次样本中，dev 4.626→4.759 ms、release 0.686→0.669 ms；完整 record 中位数维持 dev 约 564 ms、release 约 114 ms。解析器状态线性增长；RSS/HWM 样本范围重叠，不从它们声称内存无成本或存在泄漏。测量通过实际 record/store/read/diff 路径进行，详细方法与未测规模见验收；A3 的公开基线和校验材料没有重写。

## 五案例审计分层收尾（2026-09-20）

**观察项 O3 的“将历史精确行边界用于新鲜调度”问题已实测并修正于审计层。** 本阶段没有修改产品源码、FsContext、存储、报告、排序或既有回归。历史 `field_report_audit.py` 及 v1/v2 契约/fixture/校验器逐字保留，对冻结记录仍通过；新 `fresh_field_audit.py` 和 `fresh_field_contract.py` 是独立的当前采集语义门禁，不能替代历史证据校验。

- **位置与证据**：旧 `field_warning_contract.audit_warnings` 的 `end_line == line` 断言，对旧记录有效，却拒绝新调度的 unfinished/resumed 事件。上节验收曾速记 `118/118 → 117/118`；保存证据实际为 **118/118 → 117/119**，Git root 的 post-commit hook access/X_OK/ENOENT。第 118 行属于另一个 PID 的 openat。原失败叙述保留并明确更正；新审计拒绝将该 118 作为结束行。
- **实际风险**：只放宽“行号相等”会把错误 syscall、结果、路径或角色混在一起；反过来，仍把跨运行行号当成语义条件会产生调度相关假失败。现已要求本次精确 evidence、同 PID/同 syscall 配对、全 file/exec 的路径/结果、完整差异集合，以及有限且计数精确的告警、关键线索角色和排名。
- **修复证据**：新审计 **25 个正反例**，包括旧契约对正确 split 必须失败、错误语义/告警必须拒绝、单纯行号变化必须通过。历史与新鲜五案例各 **5/5**，原历史变异 **222/222**，Rust **43/43**，平台 **6/6**，专项 `FINDINGS: 0`，原告警单元 **13/13**，fmt/Clippy/冻结和扩展校验通过。上阶段失败的新鲜记录也经新语义入口 **5/5**；不能把旧审计在该输入上的预期退出 1 擦掉。
- **剩余边界（P2/观察，保留）**：审计中的 raw decoder 仅覆盖被审计工作负载，不证明完整 strace 语法。工具升级导致真实路径/调用次数/角色/告警变化时应失败后另行归因，不自动扩大允许集。真实 FS 不确定性仍要求 incomplete/rc3；Cargo 新记录零告警不能反向升级历史 Cargo 或 SPAdes。非 leader exec、复杂 namespace、PID 复用与已有 A4/A6–A9 风险均未在本阶段解决。
- **最小后续方向 / 回归要求**：维持两层独立审计，后续新增案例先登记契约和反例。产品下一项仍建议针对非 leader exec 的 PID 身份迁移先采集同步探针、建立失败回归，再讨论模型变更；不要以此审计收尾为扩大完整性声明的理由。

本阶段没有新增 P0/P1；并非新一轮全部架构复审。详见 [验收](ACCEPTANCE.md) 和 [分层设计](FIELD_AUDIT_LAYERS.md)。开始时既有 122 个受保护文件及两组历史 records 的摘要未变，外部 alpha 结论保持原样。

## 第一阶段提交记录（原文）

提交：`b6239b786264db1e6fd8ed77d5440e1be8227b52`

消息：`Add transparent noise folding evaluation`

仅以下 10 个文件，全部在 `docs/noise-folding-evaluation/`：

```text
EVIDENCE.json
EXPECTED_CLUES.md
README.md
REPRODUCE.md
RESULTS.json
RULES.md
audit.py
collect.py
test_view.py
view.py
```

预检确认均为普通 Markdown/Python/JSON 文件，无符号链接、虚拟环境、第三方源码、raw 追踪或构建产物；扫描未发现本机 home 路径或本次私有临时目录路径。隔离的 `GIT_INDEX_FILE` 暂存区在提交前只包含上述文件。提交后只同步普通暂存区中该文档目录的 HEAD 条目，原 `.idea/.gitignore` 的暂存 diff 及四个 IDE 文件字节保持原样。作者信息沿用上一个提交，未写 Git 配置；未推送。

提交后 `git status --short`：

```text
A  .idea/.gitignore
?? .idea/RunDelta.iml
?? .idea/modules.xml
?? .idea/vcs.xml
```

## 按优先级排序的重构债务

### A1 · P1：记录枚举与单条解析耦合，单个残缺目录会阻塞整个存储根

- **位置**：[store::all](../src/store/mod.rs#L64)、[resolve/delete](../src/store/mod.rs#L103)、[record 初始化](../src/capture/mod.rs#L43)、[目录创建至初次 metadata 写入](../src/capture/mod.rs#L63)。
- **证据 / 状态：代码审查推断，未进行新的故障注入。** `all()` 遇到任意目录缺 metadata、坏 JSON、不支持版本或 ID 不符立即返回 Err；`resolve()` 在筛选请求的 ID/名称之前调用 all。record 的重名检查及 delete 也依赖这一扫描。新记录先 create_dir，再设置权限/创建 raw/获取 cwd/写 metadata，这些步骤之间崩溃或出错可留下无 metadata 的目录。正常并发读取也可能撞上初始化窗口。
- **实际风险**：一个残缺记录不仅无法读，还可能使其他健康记录 show/diff、后续 record、甚至用于恢复的 delete 均失败。diff 还会把无关坏目录导致的扫描错误归到请求的左右记录，错误归属不准确。目录名就是内部 ID，但没有按 ID 单独定位的恢复通路。
- **最小修复方向**：将目录发现结果与单条解析结果分开，携带每目录错误；完整 ID 查询只验证所选记录，标签查询显式处理无法判定的项，不能静默假定不存在以绕过重名保护。创建阶段采用不会被当成已发布记录的 staging 目录或明确定义初始化项；提供范围受限的残缺项处理路径，不自动删除其他目录。
- **先写回归：需要。** 两条健康记录旁加入无 metadata、损坏 metadata、未来 schema 项，验证健康 ID 读取、坏项诊断归属、名称冲突策略；在首次 metadata 发布前中断，验证下一次运行不会因无关坏项全局失效。现有损坏测试主要比较被损坏记录本身，未覆盖这个组合。

### A2 · P1：CWD 的字符串快照缺少有效性来源，目录重命名后仍可能给出无警告的绝对路径

- **位置**：[parse::path](../src/parse/mod.rs#L96)、[resolve_context](../src/parse/mod.rs#L314)、[采集 syscall 集合](../src/capture/mod.rs#L105)、[Quality::of](../src/diff/mod.rs#L270)。
- **证据 / 状态：代码审查推断，具体进程场景未实测。** CWD 状态是 PID→可选字符串；只在 spawn、chdir/fchdir 时更新。普通 open/access 的相对路径直接拼接旧 CWD，成功 open 返回的 fd 路径注释没有用于核对。采集集合不包含 rename 系列，也无法观察外部未追踪进程重命名。只有 unresolved/context 等现有条件才警告。README 已声明目录重命名不支持，但数据模型无法表达“绝对形式、来源为可能失效的 CWD 推导”。
- **实际风险**：进程保持某目录为 cwd，目录被重命名后再次相对 open，可能被归入旧绝对路径；如果没有其他警告，metadata 仍可能是 complete。由此可能出现错误路径差异或遗漏真实路径变化。这里不是建议消除 `..` 或事后 realpath，那样会改变符号链接语义。
- **最小修复方向**：先将路径值与解析来源/不确定性分开，区分原始绝对参数、dirfd 注释、CWD 推导和 unresolved。对可用且可靠的 fd 注释核对推导，矛盾时保留双方证据并降低确定性；明确无法观察外部 rename 的边界。是否扩大 syscall 范围应另行验证，不在本次实施。
- **先写回归：需要。** 普通 fork 后目录重命名、外部进程重命名、符号链接目标切换、失败 chdir、成功 fchdir 的组合，断言路径来源与可靠性，而不只是字符串看起来绝对。现有父子 CWD 测试覆盖继承和切换，未覆盖目录身份变化。

### A3 · P1（已修复并测量）：完成检查重复扫描

- **位置**：[Completion::from_events](../src/capture/mod.rs#L262)、[record 聚合入口](../src/capture/mod.rs#L165)。
- **证据 / 状态：2026-09-20 实测。** 原最坏平方扫描已由基线 `1c6e0f7` 证实；现在一次事件遍历建立 `BTreeMap<&str, Option<&Event>>`，保留第一个成功 exec、根最后 exit、child_pid 和字典序缺失告警。O((E+S) log P+P) 是结构分析；性能数字来自真实产品采集后处理/存储路径，不是手写替代循环。
- **回归：先测试旧版，再实施。** 20 个端到端契约旧/新 dev/release 非易变输出相同；720 个排列×7 个前缀对旧谓词；15 份实际案例 trace 通过标识重放后端在旧/新 capture/store 路径结果相同。32 单元、5 平台入口、原专项 audit 无发现；没有删减既有校验、未知事件警告或持久化检查。
- **实测收益与取舍**：32k dev 检查中位数 6.17–6.37 s → 1.58–4.63 ms；release 同模式 898–962 ms → 0.458–0.686 ms。新 O(P) 状态对全部完成的记录比旧空缺失集合占用更多节点，但去掉每事件字符串复制；当前 RSS 范围重叠，不宣称总内存下降。完整矩阵、范围、无效探针和脚本见 [优化报告](performance/a3-completion-check/OPTIMIZATION.md)。
- **保留边界**：多于 128 PID、百万事件/其他分布、真实进程扇出性能、其他 ABI/冷盘未测；根选择/PID 复用既有模型不变。v1/v2/v3、schema、CWD、排序、存储校验、锁/信号/监督不变。
- **测试债务（已修复，实测）**：旧 field_report_audit 的 warnings==[] 与现有 A2 冲突，原失败已保留。收尾阶段先复现，再建立审计专用稳定分类，结合结构化事件与 raw 行证据精确限定 pkg-config 的 RPM 子进程相对 access、Git 固定 probes、Cargo CLONE_FS；不接受额外或缺失告警。13 单元、15 真实记录的 222 次拒绝变异及五案例新采集审计通过，32 Rust 单元、5 平台入口、原专项和冻结/性能复算通过。预期不确定性仍为 partial / diff 3。详见 [验收](ACCEPTANCE.md) 最后章节及 [脱敏结果](performance/a3-completion-check/optimization/AUDIT_CLOSURE.json)。旧审计逐字归档并按 v1 哈希校验，v2 非告警检测项由确定性补丁校验保持。
- **后续**：A3 本项关闭；A4 的所有权/内存物化仍保留，实施前另做规模测量及存储错误分支回归。本轮不继续扩展。

### A4 · P2：大型数据在聚合、校验和 list 路径存在多次物化与深复制

- **位置**：[aggregate](../src/diff/mod.rs#L29)、[环境复制](../src/diff/mod.rs#L161)、[逐键 cloned](../src/diff/mod.rs#L187)、[finish](../src/store/mod.rs#L195)、[records](../src/store/mod.rs#L228)、[list](../src/main.rs#L19)。
- **证据 / 状态：代码审查确认复制路径，性能影响未测量。** aggregate 对所有事件复制 key/outcome/evidence；比较每个 key 又 clone 整个 outcome→Vec<Evidence> 映射，即便结果集合完全相同；报告最终每侧只取三条证据。finish 同时保留原事件、完整序列化字节、读回字节，并重新反序列化一份事件仅求长度。records 为 list 返回 Vec<Run>，把所有记录的全部事件保留到汇总阶段；main JSON 路径还构造 summaries 与完整字符串。
- **实际风险**：高重复文件访问及多记录存储的内存峰值和分配成本被放大，list 元数据也承担全量事件常驻成本。尚无 OOM 实测，不能宣称出现泄漏。
- **最小修复方向**：先消除逐键 `.cloned()` 和完整环境 map clone，借用聚合视图；聚合可保留证据引用，在输出边界仅复制样本。list 逐记录验证并释放事件，不能为节省内存跳过摘要校验；finish 后续可用流式序列化/摘要及逐行读回验证。必要的数据所有权保留在 Run，不建议无依据引入 Arc/Event 池。
- **先写回归：需要。** 对比前后 JSON、前三条证据及损坏检测行为；大重复事件/多记录基准测峰值。优化不能只保留三个证据而丢掉原 events/raw。

### A5 · P2：输出 I/O 错误绕开 Result，正常管道关闭会触发 panic

- **位置**：[main JSON 输出](../src/main.rs#L48)、[diff JSON 输出](../src/main.rs#L67)、[report 输出](../src/report/mod.rs#L2)、[record 最后诊断](../src/capture/mod.rs#L258)。
- **证据 / 状态：本轮只读实测。** 使用已有二进制读取已有 cmake-good 记录，父进程立即关闭 stdout 管道读端，目标 RunDelta 返回 **101**，stderr 为：

```text
thread 'main' (...) panicked at .../std/src/io/stdio.rs:1166:9:
failed printing to stdout: Broken pipe (os error 32)
```

- **复现（只读已有记录）**：

```python
import subprocess
p = subprocess.Popen(["target/debug/rundelta", "--storage", STORE,
                      "show", EXISTING_RUN, "--json"],
                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
p.stdout.close()
print(p.stderr.read().decode())
print(p.wait())  # 本轮 101
```

- **实际风险**：用户用 head 等提前停止读取时，收到 panic/101 而非 CLI 定义的输出处理语义。record 最后 eprintln 若遇 stderr 写入失败，同类风险可能发生在记录已 finalized 后；这部分仅为推断，未实测。
- **最小修复方向**：输出边界接受 `io::Write` 并返回 io::Result，明确 BrokenPipe 的 CLI 策略；其余输出错误保留操作上下文。不要为隐藏 panic 统一吞掉所有错误，也不要误称目标失败。`report::render` 写 String 的 unwrap 本身不是这次故障来源。
- **先写回归：需要。** 关闭下游 stdout、非 BrokenPipe 写失败、正常 JSON/text；record 写入完成后的诊断失败应与已保存状态分开验证。

### A6 · P2：持久化 DTO、内部状态和可信性判定缺少统一的语义校验层

- **位置**：[model 数据结构](../src/model/mod.rs#L9)、[store 版本/字段校验](../src/store/mod.rs#L83)、[read_run](../src/store/mod.rs#L234)、[capture 状态组装](../src/capture/mod.rs#L177)、[Quality::of](../src/diff/mod.rs#L270)。
- **证据 / 状态：代码审查推断，不是已证明的摘要绕过。** state/completeness/kind/outcome/storage status 都是自由 String；exit_code/signal 可同时存在或都缺失。读入验证版本、摘要、事件数以及 running/ended 的部分条件，但不集中验证全部状态组合；Quality::of 对 complete 的判断不检查 warnings 或目标/采集器状态一致性，后者依赖 capture 写入时的隐含保证。finish 也只检查 running/ended/event count。未知事件 kind 在 diff aggregate 中被跳过。
- **实际风险**：后续迁移或内部调用构造出逻辑矛盾、但字节校验一致的数据时，不同模块可能作出相反判断。当前 writer 正常路径会按 warnings 降级；本轮没有发现普通截短或随机损坏能够穿过已有摘要校验。不能把能重写摘要的攻击者视为本设计承诺抵御的对象。
- **最小修复方向**：保留兼容读取 DTO，在边界转为经过校验的内部状态类型；集中验证 state/completeness、目标和采集器退出、warnings、事件版本/未知值。区分合法 unknown 与结构损坏，使用 Unknown(raw) 保留未知退出原文；不要把未知信号统一归为采集失败。Quality 从规范化状态派生，避免多个模块各写一组字符串条件。
- **先写回归：需要。** v1/v2 缺省、v3 合法失败/中断、未知信号、矛盾字段、未知事件、不同 finalization 状态的表驱动验证；不能通过更改格式把历史 unverified 直接变 corrupt。未知信号之间的 raw 变化目前不进入 exit diff，也应明确是否仅展示未知而非强行比较为相同。

### A7 · P2：监督资源没有统一的收尾结果，信号转发异常与部分 I/O 上下文被吞掉

- **位置**：[spawn/relay/wait](../src/capture/mod.rs#L126)、[join](../src/capture/mod.rs#L153)、[store 写入步骤](../src/store/mod.rs#L164)、[lock](../src/store/mod.rs#L43)。
- **证据 / 状态：代码审查推断，异常分支未注入。** relay 发送失败后尝试 SIGKILL，但两次错误都不返回主线程；`relay.join().unwrap_or(None)` 把线程 panic 当成没有中断信息。清理 child.kill/wait 错误被忽略。正常 wait 能收尾，PDEATHSIG 在 recorder 死亡时另有保护，但资源和错误状态散落于闭包/线程中。flock 任意错误都叫 storage busy；部分文件操作仅 `?`，没有 run ID、目标文件或写入阶段上下文。
- **实际风险**：低层失败时无法区分用户中断与监督失效，排查困难；未来若在 child 创建后加入新的提前返回，Child/handle/relay 的所有权约束不够显式。尚未实测因此产生孤儿，不把这一推断写成现存逃逸漏洞。
- **最小修复方向**：以小的具体监督对象或收尾函数持有 Child/pidfd/信号 handle/join handle，输出结构化的监督结果，显式保留 relay/escalation/wait 错误；不要另建插件后端或广播进程组。写入错误加阶段/路径上下文，flock 区分争用与其他 errno。保留当前 pidfd 安全边界。
- **先写回归：需要。** relay 非 ESRCH 错误、线程/等待失败的收尾与状态、pidfd 获取失败、锁非争用错误；正常 SIGTERM/SIGKILL/PTY 和目标子进程测试必须保持。测试应在隔离且可控的边界注入，不向可能复用的 PID 发信号。

### A8 · P2：展示分组字段参与业务排序，职责声明与数据依赖不一致

- **位置**：[Change.detail_group 注释](../src/diff/mod.rs#L8)、[priority](../src/diff/mod.rs#L65)、[分组赋值](../src/diff/mod.rs#L211)、[report 折叠](../src/report/mod.rs#L41)。
- **证据 / 状态：代码直接可见，未修改排序。** detail_group 注释称 presentation-only，但 priority 根据是否 Some 将相关变化移入第 10 档；report 又根据这一字段折叠前五之后的条目。diff 同时负责行为集合比较、路径拼写启发式、优先级、展示分组与从 store 错误类型映射可靠性。并非 report 反向改变 exit_code，而是 presentation 字段成为 ranking 输入。
- **实际风险**：以后仅调整展示分组也会改变前五条，难以独立验证“折叠不降权”。尤其不能直接把独立评估脚本的候选组替换 detail_group 并宣称行为未变。
- **最小修复方向**：先明确并分离观察分类、排序依据和展示策略的数据接口；若做重构，冻结原输出和原顺序，不顺带修改启发式。未来候选折叠保留原索引及独立可靠性。当前规模用具体模块/函数足够，不需要 trait 插件层。
- **先写回归：需要。** JSON 变化集合与退出码不随显示模式改变、排名契约、前五线索、完整证据回溯；现有冻结五例不得用于调参，新增反例应独立登记。

### A9 · P2：字节保真缺少从真实 CLI 到持久化再读取的跨层回归

- **位置**：[OsString CLI](../src/cli.rs#L21)、[bytes 编码](../src/model/mod.rs#L57)、[启动环境/命令/CWD](../src/capture/mod.rs#L30)、[raw 读取](../src/capture/mod.rs#L156)、[解析边界测试](../src/parse/mod.rs#L394)、[examples/run.py](../examples/run.py#L46)。
- **证据 / 状态：测试代码审查；未实测字节丢失。** command 使用 OsString、cwd/环境值使用 Unix bytes、bytes() 转义反斜杠，设计方向正确。已有 fixture 和 bytes 单元测试覆盖 0xff/控制字符的表示，但真实验收使用文本参数和空格/引号文件名，未覆盖非法 UTF-8 的 argv、cwd、文件名及环境值经过真实 strace→events→load→diff 的完整链。环境变量名与记录标签是 String，因此不是任意 Unix 字节接口，应明确这一约束。
- **实际风险**：现有局部编码测试不能证明各边界没有二次转义、歧义或路径上下文错误。raw 整体用 read_to_string，若某类输出出现非 UTF-8，会放弃整个 raw 文本的解析并告警；这不是静默成功，但可能丢失本可保留的有效事件，实际触发条件尚未验证。
- **最小修复方向**：先补真实字节输入回归，明确存储路径表示类型和序列化边界；只有复现后才决定 raw 是否需逐字节/逐行读取。不要把正确的 JSON UTF-8 校验或内部生成的 ASCII run ID 检查误当成 Unix 路径缺陷。
- **先写回归：需要，且先于实现变化。** 非 UTF-8 argv/环境值/文件名/cwd、字面 `\xff` 与单字节 FF 区分、dirfd 注释、控制字符；断言 show/diff 的可逆值、告警、原行号及目标收到的原字节。

## 观察项与已检查的边界

### O1 · 观察：当前完整性与兼容性主路径具备必要隔离，不建议为了重构删减校验

- **位置**：store.rs 178–282；diff.rs 255–335；report.rs 12–34；tests/reliability.py 29–93、128–193；tests/audit.py 137–177。
- **证据 / 状态**：本轮读代码，历史实测记录见 [ACCEPTANCE](ACCEPTANCE.md)，未重跑。metadata 接受 v1/v2/v3，事件接受 v1/v2，marker v1，报告 v4；v1/v2 无 marker 为 unverified，v3 无 marker 为 unfinalized，不把 completed 当作存储验证。存在 marker 就校验 metadata/events 摘要、字节数、事件数、ID/结束状态；完整行截短、半行、缺失、等长修改都有回归断言。读回使用已验证 metadata 字节，避免把较早扫描快照直接当成验证结果。
- **实际风险 / 边界**：raw 未校验，证据 file/line/end_line 没有验证可读取或范围有效；所以 storage verified 只证明 metadata/events 与 marker 一致，不能证明 raw 仍存在。new schema 没 marker 时返回空 events，diff 仍可能显示观察未发现/缺失，但可靠性 unknown/unfinalized 和退出码 3 是显式的。文本和 JSON 的结果语义由同一 Report 提供。最后 marker rename 成功、随后目录 fsync 失败时，函数可返回错误但 marker 已存在；不能把现有“临时文件创建失败无 marker”测试外推为所有 I/O 错误均无 marker，也不能声称 verified 等于断电持久。
- **最小方向**：保留上述契约；若以后加强 raw 可追溯性，另行增加证据可用状态，不偷换现有 storage 含义。区分写入阶段与持久性错误。
- **先写回归**：若修改，需补 raw 缺失/修改、marker 各写入阶段失败和读取并发窗口；本轮未实施。

### O2 · 观察：监督和路径安全有明确边界，未发现需要立即停工的 P0

- **位置**：capture.rs 96–155、273–399；store.rs 31–62、69–120、164–176；README 的“生命周期和锁”“解析覆盖与限制”。
- **证据 / 状态**：只读代码及既有验收。结构为主线程持有 strace Child、relay 持有该 child 的 pidfd；目标/子进程由 strace 追踪，RunDelta 不直接 wait 它们。pre_exec 设置 PDEATHSIG=SIGKILL 后核对父 PID；strace 使用 kill-on-exit；转发只对 pidfd，不调用负 PGID kill。未改变前台进程组/标准流。flock 由 File 持有，进程退出时内核释放；Drop 中 sync_all 不是解锁必要条件。异常退出后 running/pending/unfinalized 保留，不冒充正常完成。
- **实际风险 / 边界**：trusted strace 包装器、凭据变化、解除 ptrace、复杂 namespace、PID 复用、双 fork、完整 shell 作业控制未穷尽；SIGKILL 到异步清理完成之间锁可先释放。只测过仍受追踪的 setsid 子进程不等于所有守护化场景。实时信号 SIGRT_N 使用固定 kernel base 32，已验证的平台是 Linux x86_64，不应自然外推其他 Linux ABI。名称是标签，resolve 不拼接用户 key；run ID 与目录名一致检查、直接 run 目录拒绝 symlink、写临时文件 O_NOFOLLOW 有效降低误用。祖先目录/读入文件仍按 Path 跟随，未形成抵御恶意可写存储目录 TOCTOU 的完整沙箱；当前记录目录 0700 且威胁边界是用户自有存储，不把此未验证风险升格为已证实越界删除。
- **最小方向**：维持受信任后端与用户私有存储前提；未来改变威胁模型时再考虑目录 fd/openat 等约束，勿以宽泛进程组清理替代 pidfd。
- **先写回归**：如扩大保证范围，先写祖先/内部 symlink、PID 复用、权限转换、作业控制回归；本轮不实施也不声称覆盖。

### O3 · 观察：模块没有循环依赖或过度抽象；测试以用户行为为主但边界仍不完整

- **位置**：main 模块分发；capture→parse/store/model，parse→model，store→model，diff→model/store，report→diff/model；tests/acceptance.rs；tests/reliability.py；tests/audit.py；tests/field_report_audit.py。
- **证据 / 状态**：读完全部产品模块与上述测试，未发现自建插件、未验证价值的 trait/泛型体系。model 不执行 I/O；parse 接收文本、返回结构；store 执行持久化；capture 协调进程和保存，职责整体合理。diff 对 store::is_corrupt 的依赖和展示耦合见 A8，尚无依赖环。产品显式 unwrap 主要是 resolve 在 len==1 后 next、render 写 String；均有局部不变量，没有因数量而列“删除所有 unwrap”建议。测试内 expect/unwrap 与产品路径分开。真正实测的 panic 在 stdout 边界，见 A5。
- **实际风险 / 边界**：真实采集测试显式 ignored，普通 cargo test 不会自动证明信号/并发/旧格式行为；这种环境隔离合理，但验收需明确运行它们。存储故障注入覆盖了临时文件创建失败，未覆盖所有 partial write/fsync/rename 时点。单元测试的排序优先级/路径拼写断言偏实现契约，但另有真实 CLI、状态、退出码和证据断言，不能笼统称为“只测实现”。未看到仓库内 CI 工作流，不能据此推断外部没有 CI。
- **最小方向**：在每项债务前补对应用户可见回归；后续将可靠性环境条件与执行入口明确保留。无需先拆多 crate 或建插件系统。
- **先写回归**：无单独风格重构建议；覆盖缺口如下表，变更相应边界前须补。

## 测试覆盖矩阵：阅读到的断言不等于本轮执行

| 领域 | 已有测试代码/历史验收 | 本轮尚未验证或缺口 |
|---|---|---|
| 正常/非零目标退出，普通/实时信号与 128+signal | reliability realtime、audit target/collector；真实 return143/162/192 与信号分开 | 其他 Linux ABI、未知信号的端到端 raw 比较、relay 错误 |
| 生命周期 | TERM/KILL recorder，wait/tree/setsid，PTY 输入/Ctrl-C，锁释放后新记录 | wait/线程失败、PID 复用、凭据变化、启动中间窗口、完整作业控制 |
| CWD/路径 | fixture 和 C probe 覆盖 fork、失败 chdir、fchdir、dirfd；CLONE_FS 保守告警 | rename/外部变化、非 UTF-8 全链、复杂 namespace |
| 存储与并发 | 完整行/半行/缺失/等长改写，marker/metadata，flock record/delete | 无关残缺项隔离、初始发布竞态、短写/各 fsync/rename 失败点、并发删除读取 |
| 旧格式 | 显式移除 v1/v2 字段/marker，v3 验证，未来 metadata schema 拒绝 | 完整跨版本静态 fixture 矩阵、内部矛盾状态与未知 event 值 |
| 结果语义 | diff text/JSON 0/1/2/3 和三维状态；前五/全量 JSON/证据 | 本轮未重跑上述集成测试；仅 stdout BrokenPipe 做了只读探针 |
| 规模与所有权 | 小规模真实案例可运行 | E 增长曲线、峰值 RSS、多记录 list，不应以 clone 数量代替测量 |

## 本轮执行、未执行与下一步

实际执行：

1. 评估目录类型/脱敏路径检查、隔离暂存区 diff --check、仅该目录提交；IDE 内容/原有暂存 diff 核对不变。
2. 只读扫描所有产品 Rust 模块、可靠性/专项审计/常规验收/固定回归测试及相关文档。
3. 使用已有 cmake-good 记录做 stdout 关闭探针：**rc=101，Broken pipe panic**，未创建或改写运行记录。
4. `python3 -B tests/check_regression_freeze.py`：**PASS: regression set v1 inputs, assertions and recorded baseline are unchanged**。
5. 对既有跟踪文件、四个 IDE 文件、Git index 和本地 config 的内容摘要作前后核对，除新增本报告外未修改；不运行会刷新索引的普通 git status，而用可选锁关闭的状态查询。

未执行 cargo build/test/clippy、真实采集审计、故障注入或压力测试，以保持第二阶段只读。没有网络研究/第三方仓库操作、推送或配置修改。外部依赖实现、不同内核/CPU ABI、底层文件系统断电行为不在本轮代码审查验证范围。

**建议下一步先做 A1 的回归与恢复边界设计，再解决 A2 的路径确定性表达、A3 的单次集合计算。** A5 可作为独立小修复，但须先定义输出错误的 CLI 语义。任何实施均应另起任务；本报告没有实施上述重构，也未修改现有排名或冻结样例。

写入本报告后的预期 Git 状态为原四项加 `?? docs/ARCHITECTURE_REVIEW.md`；报告不暂存、不提交。
