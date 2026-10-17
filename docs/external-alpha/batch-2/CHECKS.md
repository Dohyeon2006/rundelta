# 校验、重跑与范围

日期：2026-09-20。本目录只有 17 个允许文件：3 份总说明、2 份来源/旧材料摘要、4 个 Python 脚本，以及每例的 CASE、RESULTS、summary.json、evidence.json。完整记录不在仓库；发布材料只保留有限标准化字段和局部证据坐标。

## 公开脚本的独立重跑

首次采集每例 good/repeat/bad/bad-repeat，共 8 条记录。随后使用 [README 中的公开入口](README.md#公开复现)，不提供 prepared-work，在第二个全新私人工作目录重新准备和采集 8 条记录。两个独立工作根均完整保留，重跑不改写首次材料。

实际采集输出（省去私人工作根那一行）：

```text
pip-12996 good record 0 capture complete_for_supported_events storage verified warnings 0
pip-12996 repeat record 0 capture complete_for_supported_events storage verified warnings 0
pip-12996 bad record 1 capture complete_for_supported_events storage verified warnings 0
pip-12996 bad-repeat record 1 capture complete_for_supported_events storage verified warnings 0
pip-12996 control diff 0 changes 0
pip-12996 comparison diff 1 changes 3
pip-12996 bad-control diff 0 changes 0
bracket-lib-376 good record 0 capture partial storage verified warnings 1
bracket-lib-376 repeat record 0 capture partial storage verified warnings 1
bracket-lib-376 bad record 1 capture partial storage verified warnings 1
bracket-lib-376 bad-repeat record 1 capture partial storage verified warnings 1
bracket-lib-376 control diff 3 changes 2262
bracket-lib-376 comparison diff 3 changes 2072
bracket-lib-376 bad-control diff 3 changes 242
PASS: commands/CWD equal, exactly one environment key changed, target source/tools unchanged; not a claim of reproduction success
```

两次分析均核对：pip 关键排名 2、3；CMake 新执行排名 3，旧执行消失 352。各 phase 事件数、capture/storage、目标/采集器退出码、三种比较数量及类别均相同。每次分析分别确认 **102 个私人观察文件**的摘要未改变。没有要求两批 PID、时间戳、临时路径或原始行号相同。

## 脱敏检查与负例

```bash
python3 -B docs/external-alpha/batch-2/test_redaction.py
python3 -B docs/external-alpha/batch-2/verify.py
```

新增独立检查器扫描普通文本、JSON 解码字符串、Python AST 常量，递归处理 C/strace hex、裸 hex、Unicode、八进制、JSON 多层转义、URL/HTML 和转义斜杠；检查私人绝对路径及常见凭据形状。只接受固定文件清单、小型文本文件；不接受符号链接或额外产物。`${CASE}/home/...` 是明示的隔离 fixture 路径，真实用户路径不能借此混入。

开发期间的实际失败保留：最初 8 个负例中 **7 通过、嵌套 JSON/hex 负例失败**，原因是先解码第二个反斜杠后留下额外斜杠。仅调整新增检查器的解码顺序后，8/8 通过；没有修改产品、旧审计或削弱负例断言。检查器不是通用秘密识别器，仍需固定清单和人工审阅配合。

## 旧冻结与扩展校验

```bash
python3 -B tests/check_regression_freeze.py
python3 -B docs/external-cases/pybind11-5626/verify.py
python3 -B docs/performance/a3-completion-check/verify.py
python3 -B docs/performance/a3-completion-check/verify_optimization.py
```

已有 external-alpha 扩展校验器固定要求 **40 个旧文件**，再嵌套验证原有 32/26 个文件范围。新增目录之前其原范围校验通过。本轮不修改它的文件清单、哈希或断言。

新增 `batch-2/verify.py` 先核对旧 40 个文件与开始时摘要逐字相同，然后在独立临时快照里执行**未修改的旧扩展检查器**，验证其原 40/32/26 文件范围。随后直接在新增目录后的整树执行旧检查器，要求保留退出 1 的固定清单拒绝：这是适用范围扩展导致的预期拒绝，**不是旧检查器支持新目录或整树通过**。新增 17 个文件由独立 batch-2 检查器负责，两种范围不能混称。

## 工作区保护

开始时另行保存 Git HEAD、暂存 diff、工作区既有文件摘要、Git 本地配置与原 alpha 清单；结束时核对 **149 个既有文件**摘要全部相同（含产品二进制与 tracer），暂存 diff、HEAD、原始本地 config 字节相同，Git status 只增加 batch-2。`.idea/.gitignore` 的原暂存状态、两个已有 README 修改和未跟踪 IDE 文件均保留。没有暂存、提交、推送、清理，未运行产品构建或更改已有审计。这里只有第二轮的新材料；不改写旧 alpha、验收或架构结论。

一次核对脚本误将保存的原始 config 与 `git config --local --list` 的扁平化输出比较，触发不等断言；随后比较相同形式的原始文件字节及 SHA-256，确认完全一致。没有改动配置使其通过；该次失败不属于产品或证据失败。

最终命令退出状态与摘要核对结果：

| 检查 | 结果 |
|---|---|
| 公开默认脚本第二个工作根重跑 | 退出 0；8 条记录，指标与首次相同 |
| 私人证据分析与不变性 | 两批均退出 0；各 102 个观察文件未变 |
| 新增脱敏负例 | 8/8，通过 |
| 新增 17 文件材料检查 | 退出 0；脱敏、CASE 摘要、状态、告警、排名与证据坐标通过 |
| 历史五案例冻结材料 | 退出 0；v1 输入/结果/审计与 v2 告警契约指纹通过 |
| pybind11 冻结证据与噪声复算 | 退出 0；446 keys / 712 calls / 420 paths，10 类、92 父进程实例 |
| A3 基线材料与优化材料校验 | 均退出 0；基线 27 样本，优化 81 最终 + 27 试采样本及保留失败证据通过 |
| 旧 external-alpha 40/32/26 文件范围 | 未修改的检查器在原范围快照退出 0 |
| 旧 external-alpha 校验器直接检查扩展整树 | 实际退出 1，固定清单拒绝；不计作整树通过 |
| 工作区/暂存区/产品二进制保护 | 通过；149 既有文件、暂存 diff、原始 config、HEAD 与原 status 均未变 |

本阶段未重跑 Rust 产品单元/集成测试或性能测量，未变更产品与旧测试；这里的 A3 命令是既有材料的复算校验，不是新的性能实验。
