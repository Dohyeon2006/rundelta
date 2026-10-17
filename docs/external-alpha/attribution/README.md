# 外部 alpha 采集完整性归因：SPAdes 未知未完成调用

日期：2026-09-20。本报告是增补，原 26 份 alpha 材料逐文件保留，不修改其结论、排序或记录。本次没有改变产品、capture、解析规则、测试或完整性判断。

**结论：原 SPAdes syscall 的具体名称/编号无法恢复。`???` 是 strace 未取得调用信息时的占位符，不是一个可据此新增支持的 syscall 名称。** 独立探针复现了正常线程组退出期间 `PTRACE_GET_SYSCALL_INFO → ESRCH → ???` 的机制；这是原案例的有力解释，但原始记录未保存 ptrace errno，不能把探针的已知调用当作原案例身份。没有确认可通过增加解析规则修复的 RunDelta 缺陷。

即使单独解决这条未知调用，SPAdes 仍有独立的 `CLONE_FS` 警告，按现有规则仍为采集不完整、diff=3。原 alpha 严格前五命中 1/3 的结论不变。

## 原始位置与身份边界

来源是独立重放的 **`spades/bad-v2`**，目标退出 0 的成功→成功安装路径对照，**不是**触发转换错误、退出 4 的 `bad-v3`。直接从既有私有记录只读提取：

| 日志行号 | 原始内容/含义 |
|---:|---|
| 23901–23903 | PID 74943 成功 exec `spades-corrector-core`；路径不在此公开 |
| 23927 | PID 74943 的 clone3 返回 74952；含 CLONE_THREAD、CLONE_FS、CLONE_VM 等 flags |
| 24101 | `74952 ???( <unfinished ...>` |
| 24107 | `74952 +++ exited with 0 +++` |
| 24117 | `74943 +++ exited with 0 +++` |
| 24464 | 根进程 `74687 +++ exited with 0 +++` |

74952 是线程 TID，74943 是创建它的 corrector 进程/线程组 leader；数字对应原始记录，没有以 PID 匹配其他运行。同一 TID 没有任何 `<... NAME resumed>` 行，除该未知行外只观察到其退出。调用没有名称、编号、参数、返回值；不能推成 getpid、futex、exit、exit_group、openat 或某个新 Linux syscall。

文件共 24464 行，以换行结束；未知行之后还有 363 行，所有 `+++ killed by ... +++` 的计数为 0。目标及 tracer 均为退出码 0、signal=null。没有记录器中断、根退出缺失或 collector/target 不一致告警。

**局部调用信息缺失与文件截断不同。** 日志没有表现为在未知行结束；但原 finalized 校验只覆盖 metadata/events，未覆盖 raw 日志，不能据此声称原始日志经过存储摘要验证。本次读取时新增摘要只用于此次归因定位，不能追溯保证原采集时的 raw 字节。现存 raw 用当前解析器重放，与保存的全部 15437 个事件、两条警告及总行数完全一致；该一致性增强证据链，不等于密码学证明历史 raw 未变。

## 解析器和可靠性调用链（只读审查 + 重放实测）

源位置以本轮未修改的 RunDelta checkout 为准：

1. [src/parse/mod.rs:167](../../../src/parse/mod.rs#L167)：识别 `<unfinished ...>` 后，将 `("???( ", 24101)` 放入按 PID 索引的 pending 表并继续；此时不创建 syscall 事件。
2. [src/parse/mod.rs:204](../../../src/parse/mod.rs#L204)：第 24107 行创建正常 exit 事件；没有清除或解释 pending 条目。
3. [src/parse/mod.rs:328](../../../src/parse/mod.rs#L328)：读完整个文件后 pending 仍存在，发出 `raw/trace.log:24101: unfinished call at end of trace`。**此处的 “at end” 表示解析结束仍待配对，不证明原始文件在 24101 行截断。** 没有隐藏未知行，其来源通过 warning 的文件/行号可回溯，但模型没有为该未知调用保留独立事件。
4. [src/parse/mod.rs:339](../../../src/parse/mod.rs#L339)：成功 clone3 的 CLONE_FS 独立触发共享文件系统/CWD 警告。
5. [src/capture/mod.rs:158](../../../src/capture/mod.rs#L158)、[src/capture/mod.rs:211](../../../src/capture/mod.rs#L211)：保存 parser warnings；进程完成且仍有警告时，state=completed、completeness=partial。目标与 tracer 退出均为 0，没有被误报为采集器失败。
6. [src/diff/mod.rs:270](../../../src/diff/mod.rs#L270)：partial 被表达为 capture=incomplete；存储保持 verified，比较退出 3。它仍允许浏览观察差异。

在隔离目录用 `rustc` harness 按路径引用原封不动的 model/parse 模块，依赖现有 dev rlib；输出二进制与 JSON 均在外部目录。没有修改、重建 RunDelta 或改写任何保存记录。全量重放结果见 [RESULTS.json](RESULTS.json)。

## strace 证据与机制

本机是 Linux x86_64 / Fedora 44 / kernel 7.2.5 / glibc 2.43 / GCC 16.2.1 / strace 6.16。使用本机 help 确认参数；本地 strace `syscall.c`、`strace.c` 与官方 v6.16 文件逐字节相同。

[官方 v6.16 syscall.c](https://github.com/strace/strace/blob/v6.16/src/syscall.c) 中，`get_scno` 先清空调用条目再读取寄存器/调用信息。读取失败时 `syscall_entering_decode` 打印默认条目的 `???`，而不是按编号选择 decoder；普通未知编号通常使用 `syscall_0x...` 形式。该错误分支发生在正常 syscall 过滤/输出之前，所以不能从它出现在筛选日志里推断它一定属于 RunDelta 支持的文件/进程 syscall。

[官方 v6.16 strace.c](https://github.com/strace/strace/blob/v6.16/src/strace.c) 的 `printleader` 可把尚未结束的输出补成 `<unfinished ...>`；`dispatch_event` 对 syscall stop 处理失败后继续等待线程终止。源码将线程组退出/终止期间 ptrace 无法读取线程信息列为已知场景。这是追踪后端缺少信息的表示，不是 RunDelta 将一个完整 syscall 名称解析成了问号。

Linux [`exit_group(2)`](https://man7.org/linux/man-pages/man2/exit_group.2.html) 结束线程组；[`ptrace(2)`](https://man7.org/linux/man-pages/man2/ptrace.2.html) 说明 ESRCH 可能涉及线程消失或不在可操作的追踪停止状态。不能仅凭 ESRCH 在任意场景推出唯一原因。

本次没有检查或引用 issue 评论、修复 PR 或维护者针对 SPAdes 的根因讨论。这里引用的是追踪器实现和平台接口文档。

## 独立最小探针（实测，不含 SPAdes）

[probe.c](probe.c) 使用原子变量同步：工作线程全部建立后放行，主线程确认全部进入工作阶段，再执行一小段系统调用。工作线程循环 getpid 或 open/close。主线程分两种方式结束：等待 join 后退出，或在工作线程仍运行时主动 `exit_group(0)`。没有外部 kill、超时触发、root 操作或 syscall 结果注入。

| 路径/方式 | 工作线程数（不含主线程） | 出现 `???` 的运行/总运行 | `???` 行数 |
|---|---:|---:|---:|
| RunDelta：getpid，先 join | 16 | 0/12 | 0 |
| RunDelta：getpid，直接 exit_group | 16 | 9/12 | 23 |
| RunDelta：open/close，直接 exit_group | 16 | 6/12 | 15 |
| 直接 strace：getpid，直接 exit_group | 1 | 0/24 | 0 |
| 直接 strace：getpid，直接 exit_group | 2 | 3/24 | 3 |
| 直接 strace：getpid，直接 exit_group | 4 | 15/24 | 19 |
| 直接 strace：getpid，直接 exit_group | 16 | 6/12 | 15 |
| 双层 strace：外层观察内层 ptrace | 16 | 9/12 | 9 |

首批共 132 次，全部目标/追踪器退出 0。最小成功配置是**主线程加两个工作线程**；一个工作线程 24 次未复现，这是保留的阴性结果，不证明永远不会出现。此竞态不是每次必现；不能以未出现问号判定采集器已修复。

内层使用与 RunDelta 相同的 syscall 过滤和 strace 参数，外层只追踪内层的 ptrace/wait，不跟踪其子线程，避免抢占 tracee。一次代表证据：

```text
inner line 21: 79847 ???( <unfinished ...>
outer line 5390: ptrace(PTRACE_GET_SYSCALL_INFO, 79847, 88, <ADDRESS>) = -1 ESRCH (No such process)
```

9 个问号 TID 全部匹配到同 TID 的 GET_SYSCALL_INFO/ESRCH。外层还记录启动时自 PTRACE_SEIZE 返回 EPERM；本机与官方 strace 的 `test_ptrace_seize` 明确进行这个探测，不能把它误算成采集启动失败。双层追踪会扰动调度，故另保留不嵌套的直接 strace 复现。

**探针证明该形态可在正常 exit_group 竞态中产生，未证明历史 SPAdes TID 的调用就是 getpid。** 原追踪未含 ptrace 调试事件；当前线程已经退出，不能从运行结束后的寄存器、进程名或文件系统重建身份。

## 为什么不能简单豁免退出前的 unfinished

open/close 探针还产生了 58 条 unfinished 告警，其中 15 条来自问号，另外 43 条来自已命名调用。示例 PID 78841：第 552 行 openat 访问 `/dev/null` 尚未返回，第 564 行线程退出 0。退出完成不证明该 openat 成功或失败；按正常退出自动清空所有 pending，会丢失实际访问结果不确定性。

join 对照没有问号/unfinished 告警，**12/12 仍是 partial**，原因正是 CLONE_FS。原 SPAdes `bad-v3` 本就没有问号告警，也已经 incomplete。因此“正确解析这条调用就能让 SPAdes capture 变完整”与现有规则和独立对照均不符；本轮没有通过删除警告、重写 metadata 或修改规则制造 complete 结果。

## 归因分类与优先级

| 类别 | 结论与证据强度 |
|---|---|
| RunDelta 未支持某个明确 syscall | **未证实。** 输入没有名称/编号；新增 getpid/futex 等支持不能恢复它。现有 parser 保守报警符合已知信息。 |
| 原始追踪不完整 | **局部调用信息不完整属事实；文件尾截断未见证据。** 后续 363 行与退出事件存在，原日志未纳入 finalized 摘要的边界仍保留。 |
| strace 格式/平台特性 | **后端占位符路径和同形态复现已证实。** 嵌套探针对 ESRCH 的对应关系已实测；原案例对应 errno/竞态仅为有根据的归因推断。未证明新 kernel 缺陷。 |
| 目标程序被外部中断 | **未见支持证据。** 原 root/线程均退出 0；探针在不发外部信号的正常线程组结束中复现。不能从未知行反推 SIGKILL。 |
| 仍无法判断 | 原调用编号、参数、结果；原 SPAdes 当时 GET_SYSCALL_INFO 是否确实 ESRCH；若使用不同 strace/kernel 是否消失。 |

**不建议把它作为“新增 syscall decoder/放宽完整性”的下一项产品修复。** 可安排低优先级的诊断表达改进：把“待配对调用在已观察到线程退出后仍未解析”与“文件尾可能截断”区分，关联开始/退出两处证据，并继续标记不完整；这不承诺减少信息缺口。当前没有确认应立即修复的 RunDelta 采集/解析正确性缺陷。

SPAdes 仍适合独立根因验证：#1339 的路径转换错误发生在另一个阶段，不能用成功运行里的追踪告警解释它。下一阶段应固定 Python、兼容库和输出条件，单独控制安装/CWD 路径，再分析错误原因；本轮未做该归因、未对外互动。

对 alpha 证据的影响是**归因更清楚、可靠性边界不变**：保留 1/3 严格命中、SPAdes 部分范围帮助及 diff=3，不把未知 syscall 称为已修复，不提高 alpha 通过率。

## 复现与未覆盖范围

只读使用已有 RunDelta/strace，所有新输出自动进入独立目录：

```bash
python3 -B docs/external-alpha/attribution/probe.py \
  --repo "$PWD" --strace "$PWD/target/tools/strace-6.16/src/strace"
```

可选加 `--original-raw "$RAW_TRACE" --original-show "$SAVED_SHOW"` 执行当前 parser 全量重放；变量由本地现存私有目录取得，不在文档公开。需要 gcc/pthreads 和匹配既有 dev rlib 的 rustc；依赖歧义时明确失败，不重建产品。两批次数/输出在 [RESULTS.json](RESULTS.json)，没有保存完整追踪或探针二进制。

未执行：其他 kernel/CPU/strace 版本矩阵、确定性控制内核调度、读取历史寄存器、对原 SPAdes 的嵌套 ptrace 重录、特权跟踪、原 alpha 重新排序。未运行修改构建目录的产品测试；本阶段产品和已有测试未改。

## 校验边界

旧 `../verify.py` 固定原 26 文件白名单；本目录新增文件使它在扩展树直接执行时以 AssertionError/1 退出，**这是审计清单范围冲突，不是材料内容验证通过**。未修改它或降低其断言。

`attribution/verify.py` 先核对原 26 文件的逐字节 SHA-256，再将这 26 文件原样复制到临时快照，运行未经修改的旧验证器；另外对扩展树执行明确联合文件白名单及所有内容的脱敏检查。直接旧校验的失败亦保留在 RESULTS.json。旧原件、快照及新增文件的边界均透明可复算。

```bash
python3 -B docs/external-alpha/attribution/verify.py
python3 -B tests/check_regression_freeze.py
python3 -B docs/external-cases/pybind11-5626/verify.py
python3 -B docs/performance/a3-completion-check/verify.py
python3 -B docs/performance/a3-completion-check/verify_optimization.py
```

公开 `probe.py` 在第二个全新目录完整执行：再运行 132 次，parser 全量等价性也通过。问号运行数依次为 join 0/12、group-getpid 10/12、group-open 9/12、1 worker 0/24、standalone 9/12、nested 10/12、2 workers 2/24、4 workers 10/24，全部退出 0。与首批的出现次数不同，保留为调度相关性证据，不声称固定复现率。两批合计 264 次。

最终实测校验：原 26 文件快照与扩展范围脱敏检查均退出 0；旧校验器在扩展树直接运行退出 1（六个新增文件触发原有白名单断言），保留失败原因。126 个受保护文件、原 raw 摘要及暂存区 binary diff 均未变化；没有暂存、提交、推送或对外互动。

- `python3 -B tests/check_regression_freeze.py`：退出 0；PASS: original v1 inputs/audit/results preserved; v2 changes limited to warning contract and explicit reliability assertions; v2 fingerprints verified
- `python3 -B docs/external-cases/pybind11-5626/verify.py`：退出 0；PASS: frozen files; 446 keys / 712 calls / 420 paths; 10 categories; 92 parent instances; redaction checks
- `python3 -B docs/performance/a3-completion-check/verify.py`：退出 0；PASS: nine groups, 27 samples, statistics/hashes, five calibration samples and public path scan
- `python3 -B docs/performance/a3-completion-check/verify_optimization.py`：退出 0；PASS: 81 final samples + 27 retained pilot samples; same input hashes; statistics/frames/27 reads/20 contracts; two rejected probe attempts retained; final audit closure verified; public path scan
