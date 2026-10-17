# CLONE_FS 与共享 CWD 语义归因

2026-09-20。仅新增本目录；产品代码、解析器、排序、存储格式、现有 alpha 结论和冻结材料均未修改。

**判断：值得作为下一项有边界的产品建模工作。** 当前 RunDelta 能识别共享 FS 的迹象，但没有共享 FS 对象、成员关系或跨线程 CWD 更新模型，而是禁用整次运行的历史 CWD 上下文。因此它当前不能完整建模共享 CWD；这不等于它在本次探针中输出了错误路径。90 次有同步保障的真实采集没有发现虚假绝对访问路径，成功 open 的路径证据被正确保留。

建议先建立按共享 FS 组维护的“最近观察到的 CWD 上下文”，避免无关任务被全局影响；**不把该上下文提升为当前物理绝对路径，不豁免未覆盖行为，也不以让 SPAdes diff 返回 1 为验收目标**。SPAdes 现有采集不完整等级及 alpha 严格命中 1/3 均保持不变。

## 审查范围与现有行为

| 位置 | 当前实现及影响 |
|---|---|
| [parse:275](../../../src/parse/mod.rs#L275)、[parse:307](../../../src/parse/mod.rs#L307) | clone/clone3/fork/vfork 保存 spawn、child_pid；成功调用正文含 CLONE_FS 或成功 context 操作时，将 context_uncertain 设为 true。没有将共享 flags、TGID 或 FS 组保存为独立字段。 |
| [model:10](../../../src/model/mod.rs#L10) | Event 使用 pid 字符串表示执行任务，另有 child_pid、cwd_before 和 context_uncertain；线程 TID 与进程 PID 没有独立类型或成员关系。字段同时承载共享/命名空间风险与单条路径不确定性，不能单靠它重建共享拓扑。 |
| [parse:339](../../../src/parse/mod.rs#L339) | 第一遍发现任一 context_uncertain 后设置整次运行的 shared=true。对任意事件，包括共享创建之前以及其他 fork 分支，base 都为 None，cwd_before 全部失去上下文。同一警告还涵盖 root/namespace 操作，不能把其出现直接当作“线程发生过 chdir”的证明。 |
| [parse:362](../../../src/parse/mod.rs#L362) | 无全局 shared 时，spawn 为子任务复制创建者的历史 CWD；成功 chdir/fchdir 只更新当前 PID 的 map 条目，不传播到共享者。失败切换不更新该条目。 |
| [parse:110](../../../src/parse/mod.rs#L110)、[parse:284](../../../src/parse/mod.rs#L284) | 绝对输入及有效 strace dirfd/AT_FDCWD 路径注释保留为绝对拼写；仍为相对的成功 open 可使用返回 FD 注释。没有以历史 cwd_before 拼接文件访问绝对路径。若入口注释已经解析成绝对拼写，目前不会再用返回 FD 注释交叉核验它。 |
| [parse:389](../../../src/parse/mod.rs#L389) | 未解析的相对文件/exec 路径保留 relative 表示并报警；无法取得路径的 fchdir 也会报警。相对 chdir 可保留上下文描述，但不用于证明后续文件物理路径。 |
| [capture:211](../../../src/capture/mod.rs#L211) | 目标完成但有 warning 时 completeness=partial；completed 是生命周期状态，不能当成完整采集。 |
| [report:134](../../../src/report/mod.rs#L134)、[diff:270](../../../src/diff/mod.rs#L270) | show 文本区分历史 CWD、相对不确定路径、记录到的绝对拼写；JSON 保留相同字段。diff 中 partial 对应 capture=incomplete；存储通过校验也不能使它升级。 |

审查发现的是实际覆盖缺口及过宽的上下文禁用，不是“共享 CWD 被错误当作每线程私有”后偷偷拼接错误路径：当前实现用全局禁用避免了这种错误。告警文字中的 fallback 不意味着未报警时会把历史 chdir 路径当成物理目录。

Linux 的 [clone(2) CLONE_FS 语义](https://man7.org/linux/man-pages/man2/clone.2.html)包括共享 CWD、root 和 umask；不含该 flag 时创建的是副本。FS 共享关系与线程组关系不能等同，本次非线程 clone 对照也实际验证了这一点。[unshare(2)](https://man7.org/linux/man-pages/man2/unshare.2.html) 可解除 FS 共享；成功/失败的区别必须建模。本阶段没有做有权限要求的 namespace/root 实验，也没有宣称完整捕获 umask。

## 探针与同步协议

[PLAN.md](PLAN.md) 在第一条 record 之前保存。独立 [probe.c](probe.c) 不使用第三方代码。A/payload.txt 和 B/payload.txt 分别包含 A、B，名字相同且只读；这是探针的判定依据，不是 RunDelta 文件内容快照。

1. 目标从 A 目录启动，先创建读者，使私有 FS 对照也在 A 获取上下文。
2. 读者向 ready pipe 写入一个字节，然后等待 done pipe。
3. 写者（主线程）收到 ready 后执行指定 chdir/fchdir，检查返回值及 errno，再发送 done。
4. 读者收到 done 后才执行相对 openat，读取并核对 marker。所有任务完成 join/wait 后，根进程才退出。

没有用 sleep 猜测顺序。当前 strace 过滤器不记录同步 pipe 的 read/write，因此没有把原始追踪说成含有全部同步事件；顺序由公开探针协议保证，采集中的切换返回和后续 open 证据也一致。这个实验刻意避免 CWD 切换与路径访问重叠，不能据此证明并发重叠时也可安全确定路径。

模式：pthread 使用共享 FS；fork 读者为私有 FS；clonefs 使用 `clone(CLONE_FS | SIGCHLD)`，共享 FS 但不使用 CLONE_THREAD；mixed 先 fork 私有读者，再在父任务创建并 join 一个不改 CWD 的 pthread，最后父任务切换目录。mixed 用于检验无关共享组是否影响私有读者。

normal 没有子任务。shared-stable 建立 pthread 但不切换 CWD。失败切换分别使用不存在的目录和 `fchdir(-1)`；探针把预期 ENOENT/EBADF 当作成功验证，故目标退出 0，并没有隐藏非预期失败。

## 实测结果

每组 3 次，公开脚本在第二个新目录又完整重放 3 次；两批共 **90 条真实运行记录**。所有 marker 与成功 open 路径检查通过；全部目标/tracer/record 退出 0、storage=verified。每批 12 条采集完整，33 条 partial。没有修改或删除记录来取得这些结果。

下表为首批，第二批状态、路径判定及退出码一致。基准 diff 是 normal-0→本组；重复 diff 是本组第 0→1 次。

| 场景 | CLONE_FS | 读者 marker / open 路径 | open 的 cwd_before | 采集 | 基准 diff | 重复 diff |
|---|---|---|---|---|---:|---:|
| normal | 否 | A / A/payload.txt | A | complete | 0 | 0 |
| shared-stable | 是 | A / A/payload.txt | null | incomplete | 3 | 3 |
| shared-chdir | 是 | B / B/payload.txt | null | incomplete | 3 | 3 |
| private-chdir | 否 | A / A/payload.txt | A | complete | 1 | 0 |
| shared-process-chdir | 是，非 CLONE_THREAD | B / B/payload.txt | null | incomplete | 3 | 3 |
| shared-fchdir | 是 | B / B/payload.txt | null | incomplete | 3 | 3 |
| private-fchdir | 否 | A / A/payload.txt | A | complete | 1 | 0 |
| shared-fail-chdir | 是 | A / A/payload.txt | null | incomplete | 3 | 3 |
| private-fail-chdir | 否 | A / A/payload.txt | A | complete | 1 | 0 |
| shared-fail-fchdir | 是 | A / A/payload.txt | null | incomplete | 3 | 3 |
| private-fail-fchdir | 否 | A / A/payload.txt | A | incomplete | 3 | 3 |
| mixed-groups | 是，仅在父任务另一分支 | A / A/payload.txt | null | incomplete | 3 | 3 |
| normal-access | 否 | A / A/payload.txt | A | incomplete | 3 | 3 |
| shared-access | 是 | B / B/payload.txt | null | incomplete | 3 | 3 |
| private-access | 否 | A / A/payload.txt | A | incomplete | 3 | 3 |

所有重复 diff 的 `observed_differences=false`；因此不完整样例“没有观察差异但退出 3”是正确结果。基准 diff=1 包括命令参数等观察变化，不表示文件故障；本阶段不评估或修改排序。

**路径确定性分开看：** 表中的成功 open 事件均 `context_uncertain=false`，来自本次 syscall/FD 注释的绝对拼写，不能把共享告警理解成每个路径都未知。相反，三组 access 扩展均保留：

```text
relative:payload.txt [cwd unresolved]       => success
relative:not-present.txt [cwd unresolved]   => ENOENT
context_uncertain=true
```

normal/private 的 access 虽有历史 cwd_before=A，仍不拼出绝对路径；shared 的 cwd_before=null。这个限制包含外部重命名风险，补共享模型后也不应绕过原 CWD 确定性原则。相同 marker 文件在两处都存在，所以 access 成功本身不能区分 A/B，区别由探针的 open/read 检验。

private-fail-fchdir 的 partial 来自无法解释失败 FD 的路径警告，并非 CLONE_FS；cwd_before 和后续 open 仍为 A。应在未来区分“失败调用的操作数路径未知”与“成功改变了 CWD”，但本次没有改警告或完整性判断。

## 关键证据与归因

[EVIDENCE.json](EVIDENCE.json) 保存每组首条运行的少量解码/脱敏 raw、事件字段、目标 marker 输出和 show 文本；[RESULTS.json](RESULTS.json) 保存全部组的重复结果和退出语义。完整追踪、运行记录和探针二进制仅在独立目录。

共享 chdir 的代表序列：

```text
line 5: 83768 clone3(... flags=...CLONE_FS...CLONE_THREAD... ...) = 83769
line 6: 83768 chdir("${WORK}/B") = 0
line 7: 83769 openat(AT_FDCWD<${WORK}/B>, "payload.txt", O_RDONLY) = 7<${WORK}/B/payload.txt>
```

这里为便于阅读缩略 flags，完整相关行在 EVIDENCE.json；原始行号及 PID 保留。clone 返回及 flags 证明在创建点共享 FS；探针没有执行解除共享，后续返回顺序与 B marker 支持本次行为链。不能仅凭任何一条通用警告证明任意时刻的共享成员、目录物理身份或未被追踪者的行为。

fork 对照用不含 CLONE_FS 的成功 clone 创建 83802；父任务 83801 切换到 B，子任务仍在 A 成功打开同名文件。mixed 中私有子任务 84073 仍读 A，但它的 cwd_before 被全局 shared 清为 null：这是**实际验证的作用范围过宽**。shared-process-chdir 则证明不能把 FS 共享关系只绑定到 TGID。

## 三类结论

**当前正确的保守行为（已实测）：** 不从历史 CWD 拼出文件绝对路径；共享情况下仍展示单个 syscall 可获得的路径注释；失败切换不改变实际读者结果；未知 access 保留 relative；partial 与存储完整性分开，diff=3 不掩盖观察结果。

**可有边界地改进的模型缺口（代码及探针共同支持）：** 没有 FS 组成员；共享更新没有跨成员传播；任何共享事件使整次运行、过去事件及无关分支的 cwd_before 消失。可改善的是观察上下文保留及告警范围，不能宣称共享目录的实时绝对位置已被证明。没有在本次同步矩阵确认必须紧急修复的虚假绝对路径输出。

**后端/平台或证据边界（未在本次覆盖）：** syscall 进入/退出区间可能交错，按入口行排序不证明切换完成的先后；strace 路径注释与并发 CWD/重命名之间不能自动视为原子快照；外部未追踪重命名、namespace/root 变化和缺失注释仍可能使物理路径不可知。尤其当前入口 dirfd 注释与返回 FD 注释没有交叉核验，这在无同步竞争下是否会产生矛盾未测量，不列为已复现缺陷。进程退出后的目录状态不能用来回填历史路径。

## 下一步最小建模建议（设计，不实施）

建议进入产品建模阶段，先限制于证据完整、操作不重叠的基本共享关系。应新增或明确维护：

- **任务实例与 FS 组分离**：创建者/子任务、生命周期及原始 flags 证据；成功带 CLONE_FS 的创建加入同一组，不带则复制创建时的观察上下文。TGID、PID/TID 和 FS 组分别表达；失败或未知 flags 不猜测成员。
- **每组的 CWD 观察状态**：最近可引用的上下文、来源事件/区间、版本和不确定原因。成功 chdir/fchdir 更新共享组；失败只保留尝试，不改变组状态。无法取得 fchdir 目录依据时更新为未知，不能沿用旧值冒充成功后的上下文。
- **成员变化与失效状态**：unshare 成功/失败、exec/退出、root/namespace 边界。只有已验证的转换规则才能更新关系；第一步未实现的转换仍须明确告警及保持不完整，不能简单沿用共享或私有假设。
- **证据顺序与确定性分离**：重叠的 CWD 更新/创建/访问保留不确定，不能将第一条或最后一条当作唯一生效顺序。路径输入、dirfd/返回 FD 注释、历史 CWD 三类证据不可混成“物理目录已知”。
- **分原因、分组的覆盖状态**：区分共享上下文尚未建模、真实未解析相对访问、未知 flags、退出/追踪缺失及 root/namespace 边界。完整性不能由“未见 chdir”或工具名称推断。

可先在解析期维护明确的内部状态，继续使用现有 cwd_before 表达历史上下文，不必为本实验立即改变记录格式。若未来要持久化组/flags 新字段，再单独设计兼容策略；已有记录不得因新版本模型自动升级为可信，也不得改写本次冻结证据。

必须先新增的回归应覆盖以下可观察行为，而非只验证内部 map：

1. 本次共享/私有 chdir、fchdir、无变化、失败切换及 marker 对照；补反向“子任务切换、创建者读取”和多个共享成员。
2. CLONE_FS 有/无 CLONE_THREAD；mixed 私有分支的上下文不再被无关组清空；创建失败和 clone3→clone 回退。
3. 成功/失败 unshare、成员退出、exec 边界；无法实现的边界必须维持告警，不能静默继承状态。
4. CWD 更新与 open/clone 的 unfinished/resumed 交错、两写者竞争、缺失返回/退出；无法决定顺序时不生成可信绝对路径。
5. 原 CWD 重命名、不完整 dirfd 注释、目录 FD 被关闭/复用、失败 fchdir、非 UTF-8 相对路径；新模型不得破坏既有确定性语义。
6. normal 重复 diff=0、共享范围未验证时无差异仍 diff=3；文本/JSON 同义；存储损坏语义、旧版本记录读取及原冻结材料保持。

本次只执行了第 1 项中的现有矩阵及第 2 项的基础/mixed 场景，其他是**拟增加的回归，尚未执行**，不能把它们当作已验证能力。

## 若暂不实现，告警应如何更准确

目前一句 shared filesystem state or root/namespace changes observed 加 cwd fallback disabled 容易被误读为“每个路径都错”或“程序一定改变了目录”。可以后续设计更精确的说明：

> 观察到任务共享 FS 上下文；当前模型未传播其共享 CWD 更新。保留已记录的 syscall/FD 路径证据，其余路径上下文可能不完整。见创建与切换事件证据。

同时列出已观察到的成员关系、成功/失败 CWD 变更数及未解析访问数，并将 root/namespace 风险分别解释。未观察到切换不等于证明没有切换；没有待解析访问也不等于 FS 模型已完整。以上只是文字/分类建议，本轮产品告警未改。

**SPAdes 保持 incomplete / verified / diff=3。** 本次同步探针不覆盖 SPAdes 的全部任务拓扑、并发路径访问及之前未知未完成调用；不能推断新模型将来必定使其完整，也不以本实验修改其已有报告。

## 复现与校验

Linux x86_64；本次 Fedora 44、kernel 7.2.5、glibc 2.43、GCC 16.2.1、strace 6.16。使用已有 RunDelta dev 二进制，版本及探针 SHA-256 见 RESULTS.json。

```bash
python3 -B docs/external-alpha/clone-fs-attribution/collect.py \
  --rundelta "$PWD/target/debug/rundelta" \
  --strace "$PWD/target/tools/strace-6.16/src/strace"
```

脚本创建新临时工作目录，编译探针，创建 A/B 数据，再以现有 RunDelta record/show/diff 运行；不重建产品、不覆写旧记录。目标 argv 使用独立参数，不经 shell 拼接。raw、JSON 和文本完整输出仅保留在脚本打印的私有目录。未发现预期结果时保留失败并停止，不放宽断言。

没有运行会改变产品构建目录的 Cargo 测试。未测平台矩阵、未同步访问竞争、真实重命名、unshare/exec 和权限 namespace 场景；不要将“同步控制下未发现虚假路径”外推为普遍保证。

新扩展校验同时核对原 32 文件 SHA-256、整个目录的明确文件清单、公开文本/十六进制解码后路径脱敏和结果一致性。原 26 文件校验及前一阶段 32 文件校验保持字节不变，在各自快照范围执行；两者在扩展后的整棵树直接执行均因固定清单返回 1，必须按范围冲突记录，不能冒充通过。

```bash
python3 -B docs/external-alpha/clone-fs-attribution/verify.py
python3 -B tests/check_regression_freeze.py
python3 -B docs/external-cases/pybind11-5626/verify.py
python3 -B docs/performance/a3-completion-check/verify.py
python3 -B docs/performance/a3-completion-check/verify_optimization.py
```

最终实测：新扩展校验退出 0，原 26/32 文件在各自快照范围均通过；两个旧验证器在完整扩展树均以白名单冲突退出 1，失败证据保留。第一次新扫描曾把文档中用斜线串联的术语识别为私有根目录路径，已改为原告警的正常措辞，未降低扫描断言。

- `python3 -B tests/check_regression_freeze.py`：退出 0。
- `python3 -B docs/external-cases/pybind11-5626/verify.py`：退出 0。
- `python3 -B docs/performance/a3-completion-check/verify.py`：退出 0。
- `python3 -B docs/performance/a3-completion-check/verify_optimization.py`：退出 0。

132 个受保护文件及原暂存区 binary diff 保持不变；仅新增本目录 8 个文件。没有暂存、提交、推送或对外互动。
