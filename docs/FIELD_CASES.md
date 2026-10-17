> 脱敏派生副本，原始证据保留在私有归档。以下为历史实测，未重新运行；仅替换登记的私有路径。
> Redacted derivative; originals remain in the private archive. Historical measurements were not rerun.

# 五个真实工具故障流程评估

> 本文件保留修复前的原始评估。后续修复与新实测见 [RANKING_REPAIR.md](RANKING_REPAIR.md)，不要将下述“未修复”视为当前状态。

本轮在本机实际执行五个工具流程，每例先保存正常记录，再保存一次正常重复对照，最后制造故障并保存异常记录，共 **15 份真实记录、10 组比较**。这是使用实际项目/工具的受控复现，不是五个外部用户事故或五个独立项目试用。没有修改产品解析、采集或排序代码，没有使用模拟追踪数据，也没有选择已知故障环境变量来直接提示答案。

结论：按“前五条给出可直接检查的异常对象”标准命中 **3/5**。下一方向选 **排序与降噪**，但须先修正本次发现的解析遗漏，并让失败执行尝试进入报告。暂不实施这些修改。

## 方法与实测结果

按现有文本/JSON changes 顺序取前五条，退出状态和采集完整性也占名次，但本身不算定位线索。预先声明候选路径，再人工审查是否与注入故障相关。原始 stderr 和单一故障注入用于验证事实，不能冒充 RunDelta 给出的解释。普通 ENOENT、未再执行的下游任务、随机临时文件不自动算有效线索。

| 实际流程 / 注入变化 | 目标退出码 | 前五条命中 / 首条线索 | 总差异 | 正常重复差异 | diff 退出码 |
|---|---|---|---:|---:|---:|
| pycparser C 转译 / PATH 切换到隔离 venv | 0→1 | 是，第 3 条解释器路径 | 85 | 0 | 3 |
| pkg-config 查询系统 libcrypt / 改为空搜索目录 | 0→1 | 是，第 2 条搜索目录 | 7 | 0 | 1 |
| Git 实际提交 / 用户身份配置消失 | 0→128 | 是，第 4 条 .gitconfig | 42 | 9 | 3 |
| GCC 编译真实 strace.c / 移走 defs.h | 0→1 | 否，第 127 条有相关探测；关键结果变化第 705 条 | 1238 | 2 | 3 |
| Cargo 离线构建 RunDelta / 指定不存在的 linker | 0→101 | 否，错误 linker 路径在整个报告中缺失 | 2004 | 1600 | 3 |

Cargo 第 3 条 rust-lld 消失是“未进入链接阶段”的弱线索。若将这种下游变化也计为有用，可得到宽松口径 4/5；本报告保留它，但不将其算成直接定位命中。五例不足以估计一般场景的命中率。

**15 份存储全部 verified；只有 pkg-config 比较两侧采集都完整，其余四例至少一侧 incomplete。** 完成写盘不等于完整解析。四例 diff 返回 3，文本顶部明确显示可靠性不足，JSON 同时表达 capture 与 storage。正常重复的 Python 虽然 0 差异也返回 3；pkg-config 正常重复返回 0，其余对照返回 3。这些结果不能表述为“全部通过完整采集验收”。

## 可复现命令和证据

环境为本机 Linux x86_64、strace 6.16、Python 3.14.7、GCC 16.2.1、GNU make 4.4.1、pkg-config 2.5.1、Git 2.55.0。确切版本输出、全部 argv、cwd、返回码保存在 commands.json。需要已配置的 strace 源码树、系统 pycparser 示例、libcrypt 开发元数据、Python venv 与已缓存的 Cargo 依赖。本轮没有安装软件或访问网络；脚本不自动补装依赖。

从仓库根目录执行：

```bash
cargo build --offline
RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace" \
RUNDELTA_STRACE_SOURCE="$PWD/target/tools/strace-6.16" \
python3 examples/field_cases.py
```

脚本每次创建独立 target/field-cases-*，先 good，再 repeat，再 bad；每例使用同一 cwd 和命令参数。配置/文件变更只发生在新建沙箱。Git commit 仅作用于沙箱仓库，RunDelta 仓库没有提交。GCC 使用复制的实际 strace 源码和已配置 Makefile 导出的独立 argv，不拼接 shell 执行。每次 Cargo 构建都清空该案例独立 target，避免增量构建造成对照差异。

| 案例 | 被测命令与注入 |
|---|---|
| python-venv | `python3 /usr/share/doc/python3-pycparser/examples/c-to-c.py /usr/share/doc/python3-pycparser/examples/c_files/basic.c`；PATH 从带 system-site-packages 的 venv 切换到隔离 venv |
| pkg-config | `pkg-config --cflags --libs libcrypt`；异常侧设置 `PKG_CONFIG_LIBDIR=$WORK/pkg-config/empty-pc` |
| git-identity | `git -c user.useConfigOnly=true commit --allow-empty -m 'Build environment check'`；沙箱 HOME 的 .gitconfig 在 bad 前移走 |
| gcc-header | 配置生成的 `gcc ... -c -o strace-strace.o strace.c`，完整参数见 commands.json；移走复制源码中的 src/defs.h |
| cargo-linker | `cargo build --offline --jobs 1 --manifest-path "$PWD/Cargo.toml" --target-dir "$WORK/cargo-linker/build"`；`CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER` 从 cc 变为不存在的 `$WORK/cargo-linker/missing-linker` |

本次 `$WORK` 为 `target/field-cases-sk8mi5rb`（表中绝对路径共同前缀以此缩写）。查看已有记录无需重跑构建：

```bash
WORK="$PWD/target/field-cases-sk8mi5rb"
target/debug/rundelta --storage "$WORK/records" diff gcc-header-good gcc-header-bad
# 返回 3：观察到变化，但采集不完整
target/debug/rundelta --storage "$WORK/records" diff cargo-linker-good cargo-linker-repeat --json
# 返回 3；1600 条 changes
```

实际执行摘要：

```text
FINISH python-venv: total=85, control=0, first_useful=3, top5_hit=True
FINISH pkg-config: total=7, control=0, first_useful=2, top5_hit=True
FINISH git-identity: total=42, control=9, first_useful=4, top5_hit=True
FINISH gcc-header: total=1238, control=2, first_useful=127, top5_hit=False
FINISH cargo-linker: total=2004, control=1600, first_useful=None, top5_hit=False
```

异常侧原始诊断节选（来自目标 stderr，不是 RunDelta 的根因推断）：

```text
ModuleNotFoundError: No module named 'pycparser'
Package 'libcrypt' not found
Author identity unknown
fatal: no email was given and auto-detection is disabled
strace.c:12:10: fatal error: defs.h: No such file or directory
error: linker `.../cargo-linker/missing-linker` not found
```

可复查文件：

- [评估脚本](../examples/field_cases.py)、[归档成绩及 run ID](FIELD_CASES_RESULTS.json)。
- [全部命令](../target/field-cases-sk8mi5rb/commands.json)、[机器评分](../target/field-cases-sk8mi5rb/results.json)、[逐条人工评价及证据](../target/field-cases-sk8mi5rb/assessment.json)。
- 原样报告：[Python](../target/field-cases-sk8mi5rb/python-venv-comparison.txt)、[pkg-config](../target/field-cases-sk8mi5rb/pkg-config-comparison.txt)、[Git](../target/field-cases-sk8mi5rb/git-identity-comparison.txt)、[GCC](../target/field-cases-sk8mi5rb/gcc-header-comparison.txt)、[Cargo](../target/field-cases-sk8mi5rb/cargo-linker-comparison.txt)。同名 .json 保留完整 changes 与证据行号。
- 每份原始日志位于 `$WORK/records/runs/<run-id>/raw/trace.log`；所有目标 stdout/stderr 位于 `$WORK/<case>-<phase>.stdout/.stderr`。

target 证据保存在本机且被 Git 忽略，清理 target 后这些链接会失效。可移走整个目录归档；重跑的随机路径、调度、数量和名次可能不同。仓库内 JSON 保留本次测量摘要，不伪装成可替代原始日志。

## 每例前五条和局限

下面保留实际排序，不将事后发现的线索提升到前面。“未观察到”尤其在采集不完整时不能解释为“肯定没有执行”。

### python-venv

| 排名 | 实际类别与对象（共同前缀用 `$WORK` 缩写） | 评价 |
|---:|---|---|
| 1 | Exit status：code=Some(0), signal=None → code=Some(1), signal=None | 症状 |
| 2 | Capture completeness：completed; partial; 4 warnings → completed; complete_for_supported_events; 0 warnings | 可靠性提示 |
| 3 | Executable changes：`$WORK/python-venv/isolated-env/bin/python3 (execve)`；未观察到 → success | 有效线索 |
| 4 | Executable changes：`$WORK/python-venv/system-env/bin/python3 (execve)`；success → 未观察到 | 有效线索 |
| 5 | Removed file access：`<REDACTED_TOOLCHAIN_PATH> (access)`；ENOENT → 未观察到 | 搜索噪声 |

- 误报：前五条未确认事实误报。
- 噪声：cpp 的 PATH 失败探测、停止运行后不再访问的模块与编译器路径；总共 85 条变化。
- 遗漏：正常运行中的 cpp/cc1 执行与进程创建部分被对齐空格解析遗漏。
- 无法解释：仅由路径无法证明 pyvenv.cfg 的 system-site-packages 内容；不能区分依赖未安装与依赖对当前环境不可见。

### pkg-config

| 排名 | 实际类别与对象（共同前缀用 `$WORK` 缩写） | 评价 |
|---:|---|---|
| 1 | Exit status：code=Some(0), signal=None → code=Some(1), signal=None | 症状 |
| 2 | New file access：`$WORK/pkg-config/empty-pc (openat)`；未观察到 → success | 有效线索 |
| 3 | New file access：`$WORK/pkg-config/empty-pc/libcrypt-uninstalled.pc (openat)`；未观察到 → ENOENT | 弱线索/常规回退探测 |
| 4 | New file access：`$WORK/pkg-config/empty-pc/libcrypt.pc (openat)`；未观察到 → ENOENT | 有效线索 |
| 5 | Removed file access：`/usr/lib64/pkgconfig (openat)`；success → 未观察到 | 有效线索 |

- 误报：前五条未确认事实误报。
- 噪声：libcrypt-uninstalled.pc 是正常回退探测，其 ENOENT 本身不等于故障。
- 遗漏：未选择记录环境变量，因此不能直接指出 PKG_CONFIG_LIBDIR 的值；这是本次配置的已知边界。
- 无法解释：单看文件访问不能证明搜索路径变化来自环境变量而非其他配置。

### git-identity

| 排名 | 实际类别与对象（共同前缀用 `$WORK` 缩写） | 评价 |
|---:|---|---|
| 1 | Exit status：code=Some(0), signal=None → code=Some(128), signal=None | 症状 |
| 2 | Capture completeness：completed; partial; 3 warnings → completed; complete_for_supported_events; 0 warnings | 可靠性提示 |
| 3 | Executable changes：`/usr/libexec/git-core/git (execve)`；success → 未观察到 | 失败的下游后果 |
| 4 | File access outcome changes：`$WORK/git-identity/home/.gitconfig (access)`；success → ENOENT | 有效线索 |
| 5 | Removed file access：`$WORK/git-identity/repo/.git/hooks/commit-msg (access)`；ENOENT → 未观察到 | 失败的下游后果 |

- 误报：未确认文件访问事实误报；正常对照的采集警告数变化反映解析调度敏感性，不计为身份环境漂移。
- 噪声：正常对照 9 条：8 条随机 Git 对象临时路径，1 条采集完整性计数变化。坏运行中缺少 hooks、对象写入等属于提交提前中止的后果。
- 遗漏：部分 openat 的 resumed 对齐输出未解析；元数据明确 partial。
- 无法解释：报告不采集 .gitconfig 内容，不能仅凭缺文件说明具体缺少 user.email；由 Git 的原始 stderr 和受控注入验证。

### gcc-header

| 排名 | 实际类别与对象（共同前缀用 `$WORK` 缩写） | 评价 |
|---:|---|---|
| 1 | Exit status：code=Some(0), signal=None → code=Some(1), signal=None | 症状 |
| 2 | Capture completeness：completed; partial; 4 warnings → completed; partial; 2 warnings | 可靠性提示 |
| 3 | Removed file access：`$WORK/gcc-header/strace-source/src/../bundled/linux/include/uapi/alloca.h (openat)`；ENOENT → 未观察到 | 搜索噪声/下游后果 |
| 4 | Removed file access：`$WORK/gcc-header/strace-source/src/../bundled/linux/include/uapi/asm-generic/bitsperlong.h (openat)`；ENOENT → 未观察到 | 搜索噪声/下游后果 |
| 5 | Removed file access：`$WORK/gcc-header/strace-source/src/../bundled/linux/include/uapi/asm-generic/errno-base.h (openat)`；ENOENT → 未观察到 | 搜索噪声/下游后果 |

- 误报：尚未确认前五条事实错误；显示的 ENOENT 消失是实际未再进行的头文件探测。
- 噪声：按路径字典序将 ../bundled 下的大量探测排到前面；正常对照 2 条均为随机 /tmp/cc*.s。
- 遗漏：cc1/as 与 clone3 的部分成功事件被对齐空格解析遗漏；不是 defs.h 路径丢失。
- 无法解释：没有源码依赖图/内容快照，不能自动证明哪个头文件使用是故障原因；GCC stderr 与单一文件移走验证了此例。

### cargo-linker

| 排名 | 实际类别与对象（共同前缀用 `$WORK` 缩写） | 评价 |
|---:|---|---|
| 1 | Exit status：code=Some(0), signal=None → code=Some(101), signal=None | 症状 |
| 2 | Capture completeness：completed; partial; 112 warnings → completed; partial; 14 warnings | 可靠性提示 |
| 3 | Executable changes：`<REDACTED_TOOLCHAIN_PATH> (execve)`；success → 未观察到 | 弱线索：未进入链接阶段 |
| 4 | Executable changes：`$WORK/cargo-linker/build/debug/build/anyhow-02472fc0cb68ba3c/build-script-build (execve)`；success → 未观察到 | 失败的下游后果 |
| 5 | Executable changes：`$WORK/cargo-linker/build/debug/build/generic-array-4cac162ab79b2857/build-script-build (execve)`；success → 未观察到 | 失败的下游后果 |

- 误报：正常对照第 2 条 serde_json build-script-build 新增属于已确认的事实误报：两次原始日志均成功执行，只有一次进入标准化事件。
- 噪声：正常对照 1600 条：1598 条新增/消失文件访问、1 条伪执行差异、1 条解析完整性计数差异；主要是 rustc 随机临时目录和构建产物路径。
- 遗漏：missing-linker 的 execve ENOENT 已在 events.jsonl 中，但 diff::aggregate 明确排除失败 exec；父 rustc argv 的 linker 参数也不参与跨运行比较。
- 无法解释：前五条 rust-lld/构建脚本消失只能说明构建链没有走到那里，不能定位错误链接器路径；本例没有靠匹配 PID 得出根因。

## 两个已证实的新问题

1. **resumed 系统调用的对齐空格未被解析。** 当前解析器要求精确的 `) = `，但 strace 实际输出 `<... execve resumed>)             = 0`。Cargo 正常对照中 serde_json build-script-build 被错误报告为新增：good 原始行 11062–11064 已成功执行，repeat 原始行 10959 也成功执行。前者未进入标准化事件，后者进入。对应 run ID 为 `18d6afeaaa6ea750-8247` 和 `18d6afeda79a4f46-9311`。这不是 PID 匹配问题，也不能只靠折叠噪声隐藏。解析警告确实使记录标为 partial，但未阻止不完整数据产生误导性“新增执行”条目。
2. **失败 exec 被比较层排除。** Cargo bad 的 `missing-linker`、`execve`、`ENOENT` 已存在 events.jsonl，证据为 `18d6aff066739836-10342/raw/trace.log` 第 779 行。比较层只聚合成功 exec，因此整个报告都没有这一直接线索。仅调排序不能恢复它。

GCC 关键 `src/defs.h` 的 success→ENOENT 在第 705 条，两侧原始证据均在第 77 行，见成绩文件里的 good/bad run ID。第 127 条只是同名头文件另一候选位置的失败探测。路径证据已在记录中，故本例首先暴露的是排序问题。

## 下一项工作决定

在三个候选方向中优先 **改排序降噪**，实施顺序建议如下（尚未实现或测试改善效果）：

1. 先为对齐空格和失败 exec 报告增加回归测试，修正事实遗漏。正常重复中同一个成功执行不能再因输出格式不同而被报告为新增；未知事件仍必须警告、保留证据和不完整状态。
2. 排序优先考虑稳定输入的单一 success→错误转变及可检查的失败执行。不能把所有 ENOENT 提到最前：本次 Cargo 有 79 条从 `[ENOENT, success]` 变为 `[ENOENT]` 的混合结果，多为指纹/构建输出变化，不等同 GCC 的头文件丢失。
3. 对编译器/Git 随机临时路径透明折叠，显示原始数量并可展开证据；保留 raw，不能宽泛忽略系统路径或隐藏解析失败。Cargo 正常对照的 1600 条包含 1598 条文件新增/消失、1 条事实误报、1 条完整性变化，必须分别处理。
4. 用同五例复测关键线索排名与正常对照噪声，并增加未参与设计的样例，防止硬编码 defs.h、missing-linker 等案例名称。改善目标不是已测结果。

本次 15 份已解析事件中的 unresolved relative 路径计数为 0，没有证据表明 CWD 模型是这五例的首要瓶颈；解析遗漏使这个计数不能证明路径模型完整。没有覆盖目录重命名、复杂共享 CWD、守护化等路径压力场景。精准进程匹配也不能解决已确认的分隔符遗漏、失败 exec 过滤或随机路径排序问题，因此不应排在这些修正之前。

## 验证范围与变更

评估脚本已完整执行：15 次 record 均满足正常侧返回 0、异常侧非零且非采集工具失败码 125；show 均读回 completed 和 verified；10 组 diff 的文本/JSON 返回码一致。completed 仅表示采集流程结束，不能取代 capture 完整性。每例的警告数量、事件数量、退出码见归档 JSON。这里的脚本断言通过不代表产品诊断质量通过，未命中和误报如实保留。

本轮新增评估脚本、报告和成绩摘要，更新 README/验收报告链接；没有更改 src、Cargo.toml 或 Cargo.lock，SHA-256 与本轮快照一致。未为了文档更新重跑此前单元/可靠性专项测试，其历史结果仍在 ACCEPTANCE.md，不能据此声称这些新发现已修复。未执行跨发行版/工具版本评估、外部用户独立试用或统计性能基准。

Git 仓库当前无 HEAD，产品文件本就未跟踪，无法提供可靠的阶段间 git diff --stat。未暂存、提交、发布；保留用户原有 `.idea/.gitignore` 暂存项。
