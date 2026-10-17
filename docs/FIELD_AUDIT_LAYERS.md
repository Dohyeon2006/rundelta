# 五案例审计分层

本阶段只修改审计和文档。FsContext 产品实现、排序、解析和采集生命周期保持阶段开始时的字节；历史冻结文件、原始记录和外部 alpha 结论不变。

## 两种证明，两个入口

| 层 | 输入 | 必须证明 | 不证明 |
|---|---|---|---|
| 历史冻结 | 已保存的五案例记录及冻结审计 v2 | 事件仍指向原来的精确 raw 行；原断言、fixture、哈希、告警与排名基线未改写 | 新版本重新采集必然产生相同 PID、行号或行边界 |
| 新鲜语义 | 未修改的 `examples/field_cases.py` 重新产生的 15 条记录 | 当前版本在本次调度下保留预登记的诊断语义、排名、证据、告警与可靠性状态 | 历史证据未被改写；任意工具版本/工作负载都符合本环境的契约 |

两者不能替代。`tests/field_report_audit.py`、`field_warning_contract.py`、v1/v2 manifest、fixture、变异测试和冻结校验器逐字保留。新入口是 `tests/fresh_field_audit.py`，不导入或修改历史审计。`tests/fresh_field_contract.py` 是独立的预登记数据，不能从本次 results.json 学习允许的行为。

## 新鲜采集的预登记

路径以下均相对于生成器新建的工作目录。三次运行分别为 good、repeat、bad；正常→正常和正常→异常都必须检查，不能删除 Git repeat。

| 案例 | 关键路径 / 操作 | 变化 | 进程来源 | 排名门禁 |
|---|---|---|---|---|
| Python | `python-venv/isolated-env/bin/python3` / execve | 未观察→成功执行 | 根任务 | ≤5 |
| pkg-config | `pkg-config/empty-pc` / openat | 未观察→成功访问 | 根任务 | ≤5 |
| Git | `git-identity/home/.gitconfig` / access | success→ENOENT | 摘要证据来自根任务；全事件另核对 maintenance 子任务 | ≤5 |
| GCC | `gcc-header/strace-source/src/defs.h` / openat | success→ENOENT | 根 GCC 的 cc1 子任务 | **精确第 2** |
| Cargo | `cargo-linker/missing-linker` / execve | 未观察→ENOENT（执行尝试） | rustc 或其已证实 worker 线程创建的链接器子任务 | **精确第 2** |

关键路径必须是当次 syscall/strace 可验证的绝对观察；旧 CWD 拼写、不同路径、relative/unresolved 或仅子串相似均不接受。关键事件的完整角色数量也核对：Python/pkg-config/Cargo 异常侧各 1，GCC 两侧各 1；Git 正常侧 root 3 + maintenance 3，异常侧 root 3。报告中的前三条抽样 evidence 不替代全事件检查。

告警使用稳定前缀分类，再以结构化事件、raw 证据和角色形成计数多重集合，不匹配整段可变的解释文字：

- pkg-config：每阶段恰好 1 条 `cwd.relative_access`，直接子进程执行精确的 rpm `--eval` 参数后访问 `~/.config/rpm/rpmrc`，access/R_OK/ENOENT。最近观察 CWD 必须等于案例初始目录，路径必须仍为 relative/cwd unresolved。
- Git：good/repeat/bad 恰好 **18/18/4** 条。具体 config、对象、hook 操作数、R_OK/F_OK/X_OK、结果、root/maintenance 角色与数量全部在新契约列举，不使用通配路径。
- Python、GCC、Cargo：**0/0/0**。Cargo 的历史记录要求旧版全局 CLONE_FS 告警；新鲜记录采用已验证的 FsContext 模型，必须没有该告警。这里不是允许或过滤任何告警；若再次出现旧告警、其他 FS 不确定性或未知事件，立即失败。

只允许以上声明；解析、存储、采集、生命周期、其他 CWD/FS 告警、多余或缺失告警、无对应告警的不确定事件都会失败。预期的 CWD 不确定性仍意味着 capture=incomplete，diff 必须返回 3。其余案例 capture=complete；存储在所有案例都必须 verified。正常退出为 0；失败目标及采集器退出码分别要求 Python/pkg/Git/GCC/Cargo 为 1/1/128/1/101，不把 diff 退出码当成目标退出码。

## 本次原始证据如何核验

每个 evidence 必须唯一指向本次 raw/trace.log 的有效开始/结束行，PID 必须一致。单行事件按当行解码。两行事件必须有同一 PID、同一 syscall 的 unfinished 入口与 resumed 返回；只拼接那两行，不把中间其他任务的 syscall 合进来。期间同一 PID 出现另一个 syscall/返回或退出即拒绝，信号通知可穿插。不同 PID 数值跨运行不比较，角色由本次已验证的创建关系和成功 exec/argv 确认。

所有 file/exec 事件都用独立、有限的审计解码器核对操作、返回结果、原始参数、path 和确定性；遇到不支持的被审计语法即失败。绝对参数、完全 `-xx` 编码的 fd/dirfd 注释才可提供绝对路径。设备 FD 的额外属性是返回值注释，不能当作路径证据。没有 realpath、结束时文件系统查询或 CWD 字符串补全。这个解码器不进入产品。

所有 diff 引用必须对应本次正确类别、操作、路径、结果的事件；全部变化集合与完整事件集合核对，不能只检查前五而漏掉观察。对关键线索和告警额外核对进程角色及数量。rustc worker 来源必须有显式 CLONE_THREAD 创建证据且无中间 exec；不能把任意子孙归为链接器。

文本和 JSON 的 observed differences、两侧 capture/storage、退出码、前五展示次序、值和详情分组计数必须一致。正常对照不能出现程序执行或执行尝试差异。局部证据坐标记录在本次审计结果中，不与历史数字比较，也不以坐标相同代替语义证明。

## 保留的失败及归因

上阶段验收把失败速记为 **118/118 → 117/118**；该原叙述和失败状态保留。本阶段逐条复核保存的 events/raw 后更正：实际为 **118/118 → 117/119**，均为 **root** 任务的 `access(".git/hooks/post-commit", X_OK) = ENOENT`。repeat 第 117 行是 unfinished 入口，**118 行属于另一个 PID 的 openat**，119 行才是匹配的 access resumed。不能把原速记中的 118 当成正确的事件结束坐标。

未修改的 v2 审计在 `event['evidence']['end_line'] == warning_line` 处退出 1。`field_warning_mutations.py` 依赖同一前提，也因此失败。这是真实保留的失败，不计为历史审计通过，也不删除 Git 重跑。归因仍是历史单行契约与合法调度/分行不兼容，而非允许把不同 syscall 近似视为相同。纠正的是文档速记，没有改写保存的事件或 trace。

本阶段再次执行旧审计确认该失败；对原冻结记录运行同一旧审计则通过。新回归同时要求旧契约拒绝合成的等价 split 事件、新契约接受；另有错误 syscall/结果/路径/确定性/角色/告警、错误 resumed 配对、同样行号但错误语义的拒绝测试。单纯移动行号但正确引用同一语义事件的测试必须通过。

首次开发新审计时，独立 decoder 对 `/dev/null` 的 `<char 1:3>` 返回 FD 属性过窄，拒绝了既有真实 Git trace；该失败留在本机开发日志。随后只修正审计的返回注释读取并新增反例测试：数字返回可验证，但带属性的注释不提供绝对路径。未修改产品、冻结检查、路径/角色/告警门禁或排名。五案例新鲜重采集在该开发修正和契约确定后才开始。

## 复现

依赖与冻结生成器一致：Linux、strace 6.16（支持所用 flags）、配置好的本地 strace 源码树、系统 pycparser 样例、Python venv、libcrypt 的 pkg-config 文件、Git、GCC/make 及离线 Cargo 依赖。缺失时失败并保留输出，不自动替换案例。测试命令不需要远程服务。

```bash
cargo build --locked
export RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace"
python3 -B tests/check_regression_freeze.py
# FROZEN 指向既有、未经改写的历史五案例目录；不要在其中重新生成。
python3 -B tests/field_report_audit.py "$FROZEN"
python3 -B tests/field_warning_mutations.py "$FROZEN"
python3 -B tests/test_field_warning_contract.py
python3 -B tests/test_fresh_field_audit.py
python3 -B examples/field_cases.py
# FRESH 使用本次生成器打印的 ARTIFACTS。
python3 -B tests/fresh_field_audit.py "$FRESH"
# 可选 --output 指向一个不存在的结果文件，拒绝覆盖已有文件。
```

旧审计原样在案例目录写 repair-audit.json；它不修改 records。新审计默认只写 stdout；指定 `--output` 只创建新汇总文件。原始 records/raw、第三方源码、venv 和构建结果仅留在本机工作区，不随文档提交。既有记录和历史冻结材料在本阶段前后做内容摘要核对。

## 适用边界

该分层允许证据行位置、合法调度和同一事件行边界变化，不允许工具链、工作负载、syscall、返回结果、路径语义、角色或预期告警发生变化后自动变绿。环境升级使严格契约失败时须单独归因。实际新鲜重跑与合成反例都不能证明全部 Linux 调度、PID 复用、非 leader exec 身份迁移、namespace 或任意 strace 版本。现有 SPAdes / external alpha 的 incomplete 结论原样保留。
