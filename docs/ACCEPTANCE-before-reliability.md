> 脱敏派生副本，原始证据保留在私有归档。以下为历史实测，未重新运行；仅替换登记的私有路径。
> Redacted derivative; originals remain in the private archive. Historical measurements were not rerun.

# RunDelta 阶段验收记录

验收日期：2026-09-19。范围：白皮书 v0.1 的阶段一、阶段二及当前已有可靠性能力。此次暂停功能扩展，未改动 src/、Cargo 配置或已有示例；只增加专项审计脚本、受控 C 被测程序和验收文档。

**结论：核心功能可用，阶段一通过，阶段二常规流程通过；可靠性验收未全部通过。** 新审计发现 3 个问题，未在本次验收中修复。先前“阶段二完成”应理解为功能交付，不能等同于全部异常行为已经验收。

## 1. 环境、复现和证据

实际环境：Rust 1.98.1、Cargo 1.98.1、Linux 7.2.5-200.fc44.x86_64、strace 6.16（本地源码构建）。系统 PATH 未安装 strace，使用下面的本地路径；未修改系统配置。专项 C 探针使用 SYS_open，审计脚本当前限定 Linux x86_64。

```bash
cargo build
export RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace"
cargo fmt --check
cargo test
cargo test --test acceptance -- --ignored
cargo clippy --all-targets -- -D warnings
python3 tests/audit.py
```

最后一条本次**返回 1**，脚本会打印本次目录与 FINDINGS。它并非“全绿测试”：正常行为断言通过后，仍将已实测的问题作为失败发现汇总。每次使用独立目录，不覆盖已有记录。脚本启动的进程都纳入收尾清理。

本次证据：

- [常规测试日志](../target/audit-baseline/)：fmt.txt、unit.txt、acceptance.txt、clippy.txt。
- [专项审计原始输出](../target/audit-baseline/focused-final.txt)。
- [专项运行目录](../target/audit-yomccv48/)：commands.json 保存实际命令、返回码、stdout/stderr；results.json 保存结果；records/runs 下保存 metadata、events 和 raw。
- [原三个案例本次重跑](../target/acceptance-xaqy3el3/)：执行路径、配置路径、success → ENOENT 及重复运行报告。

这些 target 文件被 Git 忽略；cargo clean 会删除。永久保留验收证据前应另行归档，不应将临时目录当作版本管理中的测试 fixture。

查看本次记录及报告：

```bash
target/debug/rundelta --storage target/audit-yomccv48/records show target-SIGTERM --json
target/debug/rundelta --storage target/audit-yomccv48/records show collector-killed --json
target/debug/rundelta --storage target/audit-yomccv48/records show cwd-tree --json
target/debug/rundelta --storage target/audit-yomccv48/records diff collector-killed collector-killed
target/debug/rundelta --storage target/audit-yomccv48/records diff normal collector-killed --json
```

第一对自比较返回 0 并警告，最后一个比较返回 1。目录名每次重跑不同，应替换为脚本打印的路径。

## 2. 五个重点的实测结果

### 2.1 目标信号终止与采集失败

下面是本次实际 metadata 和返回码的字段摘录（null 没有替换成推测值）：

| 实际触发 | record 返回码 | state | completeness | 目标 exit_code / signal | 采集器 exit_code / signal | 结论 |
|---|---:|---|---|---|---|---|
| 目标 raise(SIGTERM) | 143 | completed | complete_for_supported_events | null / 15 | null / 15 | 已验证正确区分 |
| 目标 raise(SIGINT) | 130 | completed | complete_for_supported_events | null / 2 | null / 2 | 已验证 |
| 目标 raise(SIGKILL) | 137 | completed | complete_for_supported_events | null / 9 | null / 9 | 已验证 |
| 目标 return 143 | 143 | completed | complete_for_supported_events | 143 / null | 143 / null | 与 SIGTERM 的同数值返回码可区分 |
| 外部 SIGKILL strace | 125 | capture_failed | incomplete | null / null | null / 9 | 已验证，不捏造目标状态 |
| 外部 SIGTERM recorder | 143 | interrupted | incomplete | null / null | null / 15 | 已验证，保留不完整记录 |
| 目标 raise(SIGRTMIN) | **125** | **capture_failed** | incomplete | null / null | null / 34 | **失败：目标实时信号被误判** |

实时信号的实际警告：

```text
raw/trace.log:6: unrecognized process status: +++ killed by SIGRT_2 +++
root process completion not observed; capture incomplete
collector status does not confirm target completion
```

原因经代码检查确认：退出状态解析不接受信号名中的下划线，信号映射表也未覆盖实时信号。普通信号成功不能推导全部信号都正确。工具信号返回语义是 128+signal 数值，而非向自身重发信号。

### 2.2 父子进程目录变化

真实 C 探针先进入 parent，再 fork；子进程 chdir ../child、随后 fchdir 到 fd；父进程等子进程结束后执行失败的 chdir，并打开 parent.txt。文件调用使用原始 SYS_open，避免只验证 glibc openat 的目录注释。

实际 state=completed，completeness=complete_for_supported_events，无警告。事件摘录（仅将共同绝对前缀缩写为 `<audit>`）：

```text
PID 48699  open  <audit>/cwd-tree/parent/../child/child.txt  success  raw/trace.log:9
PID 48699  open  <audit>/cwd-tree/fd/fd.txt                 success  raw/trace.log:12
PID 48698  open  <audit>/cwd-tree/parent/parent.txt         success  raw/trace.log:16
```

已验证：fork 继承、子进程改变目录不污染父进程、fchdir 根据描述符证据更新、失败 chdir 保留目录。保留 `..` 是现有路径表示策略，产品没有以事后 realpath 替换它。

固定日志单元测试另验证 unfinished/resumed 的继承顺序、CLONE_FS 和 namespace 变更时禁用回退推断。**后两项仅经过固定日志测试，尚未用真实共享 FS/命名空间进程验证。**

### 2.3 锁并发与异常退出释放

本次使用一个真实运行中的 recorder 占锁，另一个 recorder 和 delete 同时请求同一 storage，均返回 2：

```text
rundelta: storage busy: another recorder or delete operation holds the lock
```

记录器 SIGTERM 收尾后，新 record 返回 0。记录器 SIGKILL 后，新 record 也返回 0，证明锁没有因进程崩溃永久占用；不是仅检查“锁文件是否存在”。

但异常清理并未通过，实际输出：

```json
{"recorder_rc": -9, "new_record_rc": 0, "stored_state": "running", "stored_completeness": "pending", "orphan_tracer_alive": true, "orphan_target_alive": true}
```

`-9` 是 Python 对直接被信号杀死的进程返回值，不是 RunDelta 主动返回的 CLI 码。**锁释放已验证；孤儿进程自动清理没有实现。** `--kill-on-exit` 针对 strace 退出，不能证明 recorder 被杀会连带结束 strace。审计观察后已终止本次创建的孤儿进程。陈旧记录仍为 running，没有自动转为 interrupted 或重建 events。

当前锁只排斥遵守同一 flock 协议的工具写入；未测试 NFS、多用户恶意操作、断电或锁文件被外部替换。崩溃后孤儿追踪器仍存活时，锁已无法代表整个采集进程树的活跃性。

### 2.4 v1/v2 记录读取

已执行两种不同证据等级的检查：

1. 直接读取阶段一留下的真实 v1 历史记录 `target/acceptance-dt7m3fto/records` 中的 repeat-a：成功，schema=1，21 events；没有改写该历史记录。
2. 为持续复现，将新的真实采集数据在专用测试目录中转换为 v1 字段布局，移除 v2 字段，验证 show 和与 v2 的 diff。此项是**格式兼容 fixture**，不是“历史采集”。读取 environment/completeness 为 null，并在 diff 中提示未知。

v2 原样读取成功；未知 metadata 版本 0、999 返回 2。常规验收另测试损坏 JSON events 返回 2。

**未通过的数据完整性边界：**删除已保存 events.jsonl 的最后一条完整 JSON 行，保持 metadata 不变，show 仍成功，completeness 仍是 complete_for_supported_events，自比较 diff 返回 0 且无警告。实际结果：

```json
{"show_passes": true, "metadata_completeness": "complete_for_supported_events", "diff_rc": 0, "diff_warns": false, "desired_behavior_pass": false}
```

测试后已恢复文件。说明目前读取只保证可解析和已支持版本，没有核对 events 数量、结束事件或完整性校验值；“损坏记录会报错”的结论只覆盖检测到的格式错误，不能泛化为任意丢失/篡改。

### 2.5 不完整采集的 diff 提示

对 capture_failed、interrupted、running 三种实际记录，分别执行自比较。文本和 JSON 都有 warnings，即使 changes 为空、返回码为 0。实际文本输出之一：

```text
Run comparison: collector-killed → collector-killed
No differences in supported, aggregated observations.
warning: collector-killed: state=capture_failed, 3 parser warnings; inspect metadata.json
warning: collector-killed: state=capture_failed, 3 parser warnings; inspect metadata.json

Observed changes are investigation clues, not proof of a root cause.
```

完整记录对采集失败记录的比较显示 `Capture completeness` 和 `incomplete`，返回 1。因此，**已有不完整标记时提示通过**；**事后完整行截短未被标记时提示失败**。自比较会重复打印两侧相同警告，且把生命周期警告也称为 parser warnings；措辞和去重尚未完善。JSON 消费方必须读取 warnings，不能只判断返回码 0。

## 3. 白皮书逐项对照

状态含义：完成=当前范围已实现并有测试证据；部分=只覆盖子集或存在验收问题；未完成=没有交付/尚未验证；边界=明确不做，并非欠缺功能。

| 白皮书条目 | 状态 | 实际证据或剩余工作 |
|---|---|---|
| §1–3 定位、完整采集到报告闭环 | 完成 | 真实 record→parse→store→diff→report；报告不宣称根因 |
| §3.3 执行路径、配置路径、文件消失三个场景 | 完成 | 常规真实验收全部通过；报告有日志行号 |
| §3.3、§8.1 PID/时间/调度降噪 | 完成于受控案例 | 重复正常样例无差异；未证明任意并行程序稳定 |
| §4.1、§9 Linux CLI、参数/cwd/退出、子进程、支持的文件访问 | 部分 | 普通路径通过；实时信号、复杂进程上下文仍有限制 |
| §4.1、§7.1 显式启动环境变量 | 完成 | 未选择、unset、空字符串、不同值均已验收；不是子进程环境快照 |
| §4.1、§9 保存/list/show/diff/delete、文本/JSON | 完成于正常记录 | 常规集成验收通过；坏 metadata 可能阻断同根目录的其他操作 |
| §4.1、§7.1 完整性、警告、覆盖统计 | 部分 | 有事件/行数与警告；不是每个 syscall 覆盖率；完整行截短漏检 |
| §4.2 不做 GUI/AI/服务/跨平台/自研后端等 | 边界遵守 | 未新增这些功能 |
| §4.3 不恢复历史、不证明内容因果、追踪开销等限制 | 已说明 | README/报告保留这些限制，无反向承诺 |
| §5.1、§6 独立单 Cargo package、模块职责 | 完成 | 源码结构已具备，无数据库/插件后端抽象；结构来自代码检查 |
| §5.2 strace 可用性、独立参数、标准流 | 部分验证 | 缺失/不可用、特殊参数、真实采集通过；交互式 stdin/TTY 未专项测试 |
| §5.3 ID/标签、防重名、用户目录/显式目录、raw | 完成于已测范围 | 重名与路径标签已测；默认 XDG/HOME 分支只有代码检查，未独立环境测试 |
| §5.3 安全 metadata 替换和状态管理 | 部分 | fsync/rename 实现存在；异常释放实测；断电持久性、running 自动恢复未测/未做 |
| §5.3、阶段二 稳定版本格式 | 部分 | v1/v2 可读、未知版本拒绝；尚非完整语义校验或迁移系统 |
| §7.2 进程创建、exec、cwd、file、exit 与证据 | 完成于支持集合 | argv/child_pid/行号已验收；未知事件只有 raw+warning，未统一为 Unknown 标准事件 |
| §7.3 保真、errno、非 UTF-8、不确定路径 | 部分 | EACCES/非 UTF-8/截断固定日志测试；特殊路径真实测试；真实非 UTF-8 文件系统采集未专项验证 |
| §8.2 按行为聚合与结果分类、敏感信息策略 | 完成于运行级 | 环境/参数明文策略已写明；自动脱敏未实现且不是当前承诺 |
| §8.3 跨运行进程匹配 | 未完成（允许延后） | 当前集合聚合，不按 PID 匹配，不精准配对子进程，不比较子进程 argv 差异 |
| §8.4 不猜测忽略系统路径、原始证据保留 | 完成 | 无路径忽略规则；每类报告每侧最多三条证据，完整数据仍保存 |
| §9 record/diff 退出码 | 部分 | 普通信号/数值退出/失败已测，实时信号误判 |
| §10 阶段一 | 完成 | 核心三个场景、重复稳定、证据闭环均通过 |
| §10 阶段二 | 功能已交付，验收有保留 | 持久化重读、失败记录、名称/版本冲突已测；格式完整性及异常边界仍有缺陷 |
| §10 阶段三 解析边界、多进程/路径、中断 | 部分 | 常规与新增专项测试；3 个发现未修复，复杂行为仍待实测 |
| §10 阶段三 耗时/日志大小测量、五个真实案例 | 未完成 | 测试运行时间不等于性能基准；三个受控案例不算五个真实项目 |
| §10 阶段三 文档/安装/外部独立试用 | 部分 | README 有构建、依赖和命令；未完成外部用户验证、完整分发安装流程 |
| §11 必测项 | 部分 | 主要列表均有测试；实际日志截断与存储截断不同，后者存在漏检；详见上文 |
| §12–13 取舍、保留用户文件、不开新范围、不发布 | 遵守 | 本次仅验收；未提交、暂存、推送或修改系统配置 |

## 4. 实际测试结果与证据等级

| 命令/检查 | 本次实际结果 | 不能据此推导 |
|---|---|---|
| cargo fmt --check | 返回 0，无输出 | 功能正确性 |
| cargo test | 10 passed；真实集成 1 ignored | ignored 测试已经运行 |
| cargo test --test acceptance -- --ignored | 1 passed；真实完整脚本执行 | 全部异常边界已覆盖 |
| cargo clippy --all-targets -- -D warnings | 返回 0，无警告 | 没有逻辑缺陷 |
| python3 tests/audit.py | 返回 1；3 findings | 不能称专项全通过 |
| 普通路径、锁、信号、版本、不完整 diff | 真实进程/文件实测 | 所有 Linux/strace 版本行为相同 |
| CLONE_FS、namespace、跨行调度、非 UTF-8 解码 | 固定日志单测 | 真实复杂进程全部正确 |
| fsync/rename 抗断电、任意子进程清理、TTY、XDG 分支 | 仅实现/预期或未测试 | 不列为已验收 |

实际审计末尾：

```text
ARTIFACTS: <REDACTED_LOCAL_PATH>
FINDINGS: 3 realtime-signal, recorder-SIGKILL, post-save-truncated-events
```

## 5. Git diff 摘要

当前仓库**没有 HEAD 提交**，且产品源码、Cargo 文件、README、examples、tests 都是 untracked。因此不能诚实地提供“相对上一阶段 commit 的 diff”。

实际命令结果：

```text
git rev-parse --verify HEAD
fatal: Needed a single revision

git diff --stat
[无输出：未暂存的 tracked 文件无差异]

git diff --cached --stat
 .idea/.gitignore | 10 ++++++++++
 1 file changed, 10 insertions(+)
```

`.idea/.gitignore` 是本次之前就存在的用户暂存项，本次未修改 index。不能把普通 git diff 的空输出理解为没有项目代码。

此次验收改动范围：新增 tests/audit.py、tests/fixtures/audit_probe.c、docs/ACCEPTANCE.md；README.md 增加验收入口及缺陷说明。src/、Cargo.toml、Cargo.lock、examples/ 和已有测试未修改。完整项目的未跟踪文件清单、按新文件计算的行数摘要及 no-index patch 保存在 [Git 审计输出](../target/audit-baseline/git-summary.txt) 与 [项目新增内容 patch](../target/audit-baseline/project-untracked.patch)。该 patch 以 /dev/null 为基线，不是阶段间 diff。

## 6. 已知限制与暂停点

验收失败项按修复优先级：事件文件丢失检测；SIGKILL recorder 后的进程树生命周期；实时信号状态解析。此次只记录问题，未修复或扩展功能。

其他限制：无跨运行精准进程配对；不比较子进程 argv 行为差异；CLONE_FS/命名空间只保守警告；不处理 PID 复用、目录重命名、全部 dirfd 和 shebang 加载；不采集文件内容或所有 syscall；不提供自动脱敏、重放、崩溃恢复或完整性哈希。metadata 损坏可能使同一存储根目录其他记录也无法解析/删除。差异证据是排查线索，完整性标志也只覆盖支持集合。

这里停止。后续应先修复已发现的可靠性问题并重跑相同验收，再讨论新增功能。
