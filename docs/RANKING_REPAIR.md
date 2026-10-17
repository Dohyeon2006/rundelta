> 脱敏派生副本，原始证据保留在私有归档。以下为历史实测，未重新运行；仅替换登记的私有路径。
> Redacted derivative; originals remain in the private archive. Historical measurements were not rerun.

# 解析与报告排序修复验收

本轮已修复对齐输出漏解析和失败 exec 漏报，并实现保留全部观察结果的排序与文本折叠。没有修改 capture 生命周期、存储格式或进程匹配模型，没有新增依赖。原评估保留在 [FIELD_CASES.md](FIELD_CASES.md)，本报告记录修复后的实测。

## 先复现，再修复

新增五个回归测试，修复前 **0 通过、5 失败**，原始失败输出保存在 [regressions-before.txt](../target/ranking-repair/regressions-before.txt)：

- 对齐的 execve/clone/open 结果丢失，证据与子进程 CWD 无法保留。
- 参数字符串里含 `) = ` 时不能用简单文本分割决定返回值边界。
- 失败 exec 被排除，报告错误地返回无差异。
- 稳定输入的 success→EACCES 被探测和下游任务排到后面。
- 同一成功 exec 的普通行与 resumed 行产生不同解析结果。

修复后以上五项通过。再增加五项测试覆盖临时路径规则边界、折叠保留完整 JSON/证据/不完整状态、临时路径真实失败不能折叠、混合成功与失败探测不能抢占单一结果变化、失败后成功的 exec 不能被称为执行消失。默认单元测试合计 **23 通过**。

解析器现在定位引号外且括号配平的调用结束位置，再允许返回值等号前的对齐空白；不修改原始日志。未知、截断、缩略和未匹配的 resumed 仍警告。成功 exec 仍为 `Executable changes`；只要比较对象包含失败结果，使用 `Execution attempt changes`，同时保留 errno、成功结果和证据，避免将执行尝试称为成功执行。

## 实测五例

重新运行同一脚本，每例依次 good、正常 repeat、bad，共 **15 份新记录、10 组比较**。全部存储 verified。直接线索前五条命中从 **3/5 变为 5/5**，五例都在第 2 条。没有把目标 stderr 混入 RunDelta 的差异评分，也没有使用 --env 注入答案。

| 案例 | 修复前直接线索 | 修复后 | 正常重复原始差异（前→后） | 修复后文本条目/分组数 | 故障比较退出码 |
|---|---|---|---|---:|---:|
| Python 隔离 venv | 第 3 条 | 第 2 条：新解释器 | 0→0 | 0 | 1 |
| pkg-config 搜索目录 | 第 2 条 | 第 2 条：新搜索目录 | 0→0 | 0 | 1 |
| Git 用户配置丢失 | 第 4 条 | 第 2 条：.gitconfig success→ENOENT | 9→8 | 7 | 1 |
| GCC 必需头文件丢失 | 同名探测第 127；关键变化第 705 | 第 2 条：src/defs.h success→ENOENT | 2→2 | 2 | 1 |
| Cargo 错误链接器 | 整份报告无 missing-linker | 第 2 条：执行尝试 ENOENT | 1600→1598 | 724 | 3 |

正常重复的五例均没有执行或执行尝试差异。此前 Cargo 的 serde_json 构建脚本伪执行差异不再出现。Cargo 原始差异仍有 1598 条：**876 条折叠成 2 个分组，722 条单独展示，共 724 个条目/分组**，不是把 1598 条事实删成 724 条。Git 折叠 3 条为 2 组，8→7 个展示项，改善有限；GCC 只有 2 条，为保证前五条完整展示而未折叠。没有将折叠后的数量充当“原始差异减少”。

Python/Git/GCC 原有格式解析警告已消失；pkg-config 保持完整。Cargo good/repeat/bad 各剩 **1 条** `shared filesystem state or root/namespace changes observed; cwd fallback disabled`。这是当前 CWD 模型的保守边界，仍返回 incomplete / diff 3，没有隐藏警告来通过验收。前四组正常重复退出码依次 0、0、1、1，Cargo 正常重复为 3。

修复前后是独立真实执行，工具调度、随机路径以及 Cargo 构建的 RunDelta 源码有所变化，所以总事件/差异数不是严格单变量性能对照。为单独确认比较层作用，还用新程序只读比较了原来的 15 份已保存记录，全部继续 verified；原来的 GCC 与 Cargo 直接线索均升到第 3 条（前面还有采集完整性条目），见 [旧记录重排结果](../target/ranking-repair/old-records-reranked.json)。旧记录的解析遗漏和 partial 状态不会被自动修补，diff 不重解析 raw；恢复遗漏需要重新采集，未来若做重新解析必须另行设计版本与证据策略。

### 修复后实际前五条

下表以 `$WORK` 缩写 `target/field-cases-aden7dm4` 的绝对路径，顺序直接来自 JSON，未作人工提升。首次有效线索之外，其他条目仍可能是故障后果或噪声。

#### python-venv

| 排名 | 类别 | 对象 | 结果 |
|---:|---|---|---|
| 1 | Exit status | `—` | code=Some(0), signal=None → code=Some(1), signal=None |
| 2 | Executable changes | `$WORK/python-venv/isolated-env/bin/python3 (execve)` | 未观察到 → success |
| 3 | New file access | `$WORK/python-venv/isolated-env/lib/python3.14/site-packages (openat)` | 未观察到 → success |
| 4 | New file access | `$WORK/python-venv/isolated-env/lib64/python3.14/site-packages (openat)` | 未观察到 → success |
| 5 | New file access | `$WORK/python-venv/isolated-env/pyvenv.cfg (openat)` | 未观察到 → success |

#### pkg-config

| 排名 | 类别 | 对象 | 结果 |
|---:|---|---|---|
| 1 | Exit status | `—` | code=Some(0), signal=None → code=Some(1), signal=None |
| 2 | New file access | `$WORK/pkg-config/empty-pc (openat)` | 未观察到 → success |
| 3 | Removed file access | `/usr/lib64/pkgconfig (openat)` | success → 未观察到 |
| 4 | Removed file access | `/usr/lib64/pkgconfig/libcrypt.pc (openat)` | success → 未观察到 |
| 5 | New file access | `$WORK/pkg-config/empty-pc/libcrypt-uninstalled.pc (openat)` | 未观察到 → ENOENT |

#### git-identity

| 排名 | 类别 | 对象 | 结果 |
|---:|---|---|---|
| 1 | Exit status | `—` | code=Some(0), signal=None → code=Some(128), signal=None |
| 2 | File access outcome changes | `$WORK/git-identity/home/.gitconfig (access)` | success → ENOENT |
| 3 | Executable changes | `/usr/libexec/git-core/git (execve)` | success → 未观察到 |
| 4 | Removed file access | `/etc/localtime (openat)` | success → 未观察到 |
| 5 | Removed file access | `$WORK/git-identity/home/.gitconfig (openat)` | success → 未观察到 |

#### gcc-header

| 排名 | 类别 | 对象 | 结果 |
|---:|---|---|---|
| 1 | Exit status | `—` | code=Some(0), signal=None → code=Some(1), signal=None |
| 2 | File access outcome changes | `$WORK/gcc-header/strace-source/src/defs.h (openat)` | success → ENOENT |
| 3 | Executable changes | `/usr/bin/as (execve)` | success → 未观察到 |
| 4 | Execution attempt changes | `<REDACTED_TOOLCHAIN_PATH> (execve)` | ENOENT → 未观察到 |
| 5 | Execution attempt changes | `<REDACTED_TOOLCHAIN_PATH> (execve)` | ENOENT → 未观察到 |

#### cargo-linker

| 排名 | 类别 | 对象 | 结果 |
|---:|---|---|---|
| 1 | Exit status | `—` | code=Some(0), signal=None → code=Some(101), signal=None |
| 2 | Execution attempt changes | `$WORK/cargo-linker/missing-linker (execve)` | 未观察到 → ENOENT |
| 3 | New file access | `$WORK/cargo-linker/build/debug/.fingerprint/proc-macro2-a411f7480bd0f42c/output-build-script-build-script-build (openat)` | 未观察到 → success |
| 4 | New file access | `$WORK/cargo-linker/buildCFmeen/CACHEDIR.TAG (openat)` | 未观察到 → success |
| 5 | Executable changes | `<REDACTED_TOOLCHAIN_PATH> (execve)` | success → 未观察到 |

## 排序与折叠契约

方向为 left→right，不推断因果。顺序为：退出状态；采集完整性变化；初始 CWD/命令/选定环境；左侧唯一结果 success 变为右侧只有失败；右侧失败 exec 尝试；新增成功执行；新增普通文件访问；消失成功执行；其他结果/执行尝试变化；其余访问；可折叠细节。同一层稳定排序。混合 `[ENOENT, success]→[ENOENT]` 不等于单一 success→错误，不抢占前面的直接线索。

仅对单侧出现的文件访问标记 `detail_group`：全部结果非 success 的探测，或符合窄规则的临时路径候选。临时规则公开为路径段 `rustc`、`rmeta`、`.tmp` 后恰好 6 位 ASCII 字母数字；`/objects/` 下文件名 `tmp_obj_` 加 6 位字母数字；`/tmp/` 下 `cc` 加 6 位字母数字且以 `.s` 结束。它们只是展示启发式，不证明文件临时、无关或两条路径相同。

前五条永远完整展示。后续带 detail_group 的条目在文本末尾按组折叠，显示数量、前三个原始排名和 `--json` 查看方式。JSON **完整保留排序后的 changes、原路径、结果及既有证据样本**，不合并路径身份、不删除 raw、不改变 observed_differences 或退出码。每个结果集合沿用原来的最多 3 个证据样本，完整事件在 show/events.jsonl，完整追踪在 raw；没有宣称报告包含全部原始证据行。

以下边界仍存在：

- PATH 中大量失败执行尝试会显示，可能挤占其他线索；没有将所有失败尝试当作根因。
- 临时命名模式只覆盖部分工具输出；Cargo `.tmp*.temp-archive`、文件尾 `.tmp<hex>` 等仍可能逐条展示。不泛化到所有 `/tmp` 或系统路径。
- 新出现的失败配置访问有时属于细节组；前五条以外可能被折叠，用户必须能在 JSON 查看。没有自动证明某个路径无关。
- 跨运行仍为集合比较，没有精准进程匹配；共享 FS 仍限制 CWD 推断；同一路径不证明内容相同。
- 五例是设计输入，不是独立泛化验证。测试中换用了 EACCES、settings.ini、/tools/unavailable 等对象验证非硬编码；尚未新增外部独立项目试用。

## 格式与兼容性

metadata 仍为 v3、events 为 v2、finalized.json 为 v1；没有改变存储摘要或完成标记。diff JSON 升为 **v4**：Change 增加 nullable `detail_group`，包含失败执行尝试的新类别，changes 顺序采用上述规则；错误报告也使用 v4。消费报告的程序应更新 schema 检查。读取 v1/v2 历史记录仍为 unverified，不重写；损坏仍返回 2，不完整/未验证返回 3，完整无差异 0，完整有差异 1。三维可靠性语义与之前相同。

## 复现和测试结果

环境与依赖条件见 [原评估](FIELD_CASES.md)。从项目根目录执行：

```bash
cargo test
cargo build --offline
cargo fmt --check
cargo clippy --all-targets -- -D warnings
RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace" cargo test --test acceptance -- --ignored
python3 tests/audit.py
RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace" python3 examples/field_cases.py
# 将最后输出的 ARTIFACTS 路径作为下面参数；本次为：
python3 tests/field_report_audit.py target/field-cases-aden7dm4
```

实际结果：

- 默认单元测试 23 通过；2 项平台集成测试默认 ignored，随后显式运行 **2/2 通过**，其中可靠性脚本 **8/8 通过**。
- `cargo fmt --check` 与 `cargo clippy --all-targets -- -D warnings` 通过。
- 原 `tests/audit.py` 检测项未删除或降低断言，实测 **FINDINGS: 0**。覆盖实时/普通信号、正常返回 143、采集器被杀、记录器 TERM/KILL 与孤儿、锁并发释放、父子 CWD、历史版本读取、截短检测和不完整 diff。
- 五例脚本退出 0；新增字段审计退出 0，逐例断言线索前五、正常对照无伪执行差异、存储校验、全部警告、文本/JSON 退出语义与折叠计数。Cargo 只接受明确的共享 FS 警告，没有放宽为“任意警告均可”。

证据：[单元测试](../target/ranking-repair/unit-after.txt)、[平台集成](../target/ranking-repair/acceptance-after.txt)、[原专项审计](../target/ranking-repair/audit-after.txt)、[五例运行](../target/ranking-repair/field-after.log)、[五例审计](../target/ranking-repair/field-audit.txt)、[成绩/运行 ID/前五条](RANKING_REPAIR_RESULTS.json)。

本次记录根为 `target/field-cases-aden7dm4/records`，每例原样 `*-comparison.txt/.json`、`*-control.txt/.json`、stdout/stderr、commands.json 和全部原始记录留在同一目录。例如：

```bash
target/debug/rundelta --storage target/field-cases-aden7dm4/records diff cargo-linker-good cargo-linker-bad --json
# 返回 3；第 2 条 Execution attempt changes 为 missing-linker ENOENT
```

target 被 Git 忽略，清理 target 会删除本地证据；可复现脚本和精简成绩保留在项目文件内。本轮没有进行断电/文件系统持久性故障注入、跨平台/内核验证或性能基准，之前记录的生命周期边界仍适用。

## 文件改动和 Git 状态

产品改动仅 `src/parse/mod.rs`、`src/diff/mod.rs`、`src/report/mod.rs`，分别负责正确解析、排序/尝试事件、透明文本折叠及其回归测试。新增 `tests/field_report_audit.py`、本报告及结果 JSON，更新 README 和历史验收入口。capture、store、model、CLI、Cargo.toml/lock 与本轮开始的 SHA-256 快照一致；无新增依赖。

仓库无 HEAD 且产品文件原本未跟踪，无法提供有效的提交间 git diff --stat；工作树改动不能用空 git diff 冒充“没有改动”。本轮未暂存、提交、发布，原有 `.idea/.gitignore` 暂存项保留。
