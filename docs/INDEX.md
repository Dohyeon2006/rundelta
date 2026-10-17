> 本地证据路径说明：本文及本索引所链接历史文档中的 `target/`、本机实验目录和未发布原始日志路径，仅用于说明原本地复现实验位置；这些产物不随公开版提供，不是可访问的公开证据链接。公开可核验材料以仓库实际收录的文件和公开派生合同为准，不能据本地路径推断已发布原始记录。
> Local evidence paths: `target/`, local experiment directories and unpublished logs describe original local reproductions. Their artifacts are not shipped with this public edition and are not accessible public evidence links.

> 公开树说明（2026-10-16）：本树引用的 ACCEPTANCE-before-reliability、FIELD_CASES、HOLDOUT_RESULTS、RANKING_REPAIR 及对应结果 JSON 为脱敏派生副本；原始证据保留在私有归档，未重新实测。下文关于私有树、历史路径或发布阻塞的结论属于当时状态，不代表本公开副本仍含这些路径。公开派生合同见 [REGRESSION_SET_PUBLIC.json](REGRESSION_SET_PUBLIC.json)；原 v1 原件校验仍属私有树。
> Public references point to redacted historical derivatives, not fresh measurements. Original v1 evidence verification remains private; the public archive has a separate integrity contract.

# 文档分类索引 / Documentation index

本索引按用途整理现有资料。链接保留原路径；源码、测试、探针、冻结清单和历史证据不因分类而移动或改写。目录中的 JSON、CSV、Python、C 文件与相邻文档组成同一组材料，不应只保留 Markdown 而删除其配套文件。

This index groups existing material by purpose without moving or rewriting source, tests, probes, frozen manifests, or historical evidence. Keep supporting JSON, CSV, Python, and C files with their documents.

## 阅读顺序与语义优先级 / Reading order and authority

1. 日常使用先读[项目 README](../README.md)。内部开发规则未随公开版发布。
2. 文件系统上下文以 [FsContext 模型](FS_CONTEXT_MODEL.md)、[解析器](../src/parse/)及其回归测试为准。历史资料中的全局 `CLONE_FS` 回退、旧测试数量和旧代码行号只描述当时版本，不能覆盖当前合同。
3. 验证现版本时区分冻结历史证据与新鲜采集，见[五案例审计分层](FIELD_AUDIT_LAYERS.md)。旧版保存记录保留其原 warning、可靠性分类和原始证据坐标，不能用新 parser 追溯升级。

Start with the project README. Internal engineering rules are not included in this public edition. For filesystem-context semantics, the parser, its tests, and `FS_CONTEXT_MODEL.md` are authoritative. Historical global-`CLONE_FS` descriptions, test counts, and source locations are dated snapshots. Preserving frozen evidence and validating fresh captures are separate checks; new parser behavior must not reinterpret saved records.

## 当前合同与实现状态 / Current contracts and implementation status

| 入口 / Entry | 用途与边界 / Purpose and boundary |
|---|---|
| [FS_CONTEXT_MODEL.md](FS_CONTEXT_MODEL.md) | 当前共享、复制、分裂及不确定性模型；不等于物理路径身份保证。 / Current sharing, copying, splitting, and uncertainty model; no physical path identity guarantee. |
| [REFACTOR_DECISIONS.md](REFACTOR_DECISIONS.md) | 已接受的产品取舍、capture 软上限和后延边界。 / Accepted product decisions, capture soft limits, and deferred boundaries. |
| [REFACTOR_STATUS.md](REFACTOR_STATUS.md) | 2026-09-21 重构交付状态、验收与保留债务；本身不是新的 schema 合同。 / Refactor status, validation, and retained debt as of 2026-09-21; not a new schema contract. |

## 测试与冻结证据 / Tests and frozen evidence

| 入口 / Entry | 用途与边界 / Purpose and boundary |
|---|---|
| [FIELD_AUDIT_LAYERS.md](FIELD_AUDIT_LAYERS.md) | 区分历史证据完整性与新鲜语义验收。 / Separate historical evidence preservation from fresh semantic validation. |
| [REGRESSION_SET.md](REGRESSION_SET.md) · [REGRESSION_SET.json](REGRESSION_SET.json) | 五个冻结案例、摘要与使用限制；不是排序调参材料。 / Five frozen regression cases, hashes, and restrictions; not ranking-tuning data. |
| [INCOMPLETE_CAPTURE.md](INCOMPLETE_CAPTURE.md) | 旧版 Cargo 记录的 incomplete 与 diff=3 示例；不是当前重新采集的预期。 / Historical incomplete Cargo records with diff=3; not the expected result of a new capture. |
| [tests/](../tests/) · [tests/full_gate.py](../tests/full_gate.py) | 测试合同和统一门禁入口，含可明确报告跳过原因的环境检查。 / Test contracts and the unified gate, including explicit environment skips. |
| [examples/](../examples/) · [examples/field_cases.py](../examples/field_cases.py) | 可执行示例与冻结案例生成器，也是验收依赖。 / Executable examples and the frozen case generator, also used by acceptance checks. |

冻结材料包含源文件字节、路径引用、证据文件和行号。不要为了目录整洁移动 `src/`、`tests/`、`examples/`，或改写 docs 下被 manifest 引用的文件；需要迁移时应单独评审引用和冻结合同。`target/` 也混有本地工具和历史实验记录，不能按目录名整体当作可丢弃缓存。

Frozen material includes source bytes, path references, evidence files, and line coordinates. Keep source, test, example, and manifest-referenced document paths in place; any migration needs a separate contract review. `target/` also contains local tools and historical experiments, so its name alone does not make all contents disposable.

## 历史验收与调研 / Historical acceptance and research

以下材料用于追溯决策、复核旧结果，不以文件中的“当前”“尚未”等措辞推断今天的状态。

Use these records to trace decisions and review past results. Read statements such as “current” or “not yet” in the context of their original dates.

| 入口 / Entry | 内容 / Contents |
|---|---|
| [ACCEPTANCE-before-reliability.md](ACCEPTANCE-before-reliability.md) | 可靠性修复前的验收与失败发现。 / Acceptance findings before the reliability fixes. |
| [ACCEPTANCE.md](ACCEPTANCE.md) | 可靠性、CWD、存储与 CLI 的阶段验收记录。 / Dated reliability, CWD, storage, and CLI acceptance records. |
| [ARCHITECTURE_REVIEW.md](ARCHITECTURE_REVIEW.md) | 2026-09-20 架构审查及阶段修复，不是重构后的现状清单。 / Architecture review and staged fixes from 2026-09-20, not an inventory of the refactored implementation. |
| [FIELD_CASES.md](FIELD_CASES.md) · [FIELD_CASES_RESULTS.json](FIELD_CASES_RESULTS.json) | 第一批五个真实工具流程的原始评估。 / Original evaluation of five real-tool workflows. |
| [RANKING_REPAIR.md](RANKING_REPAIR.md) · [RANKING_REPAIR_RESULTS.json](RANKING_REPAIR_RESULTS.json) | 解析遗漏、失败执行尝试与排序修复后的验收。 / Validation of parsing, failed execution attempts, and ranking repairs. |
| [HOLDOUT_PLAN.md](HOLDOUT_PLAN.md) | 第二批五例的执行前登记。 / Preregistered plan for the second set of five cases. |
| [HOLDOUT_EVALUATION.md](HOLDOUT_EVALUATION.md) · [HOLDOUT_RESULTS.json](HOLDOUT_RESULTS.json) | 独立案例结果、失败案例和能力限制。 / Holdout results, unsuccessful cases, and capability limits. |
| [noise-folding-evaluation/](noise-folding-evaluation/) | 独立噪声折叠设计实验：[说明](noise-folding-evaluation/README.md)、[规则与反例](noise-folding-evaluation/RULES.md)、[预登记](noise-folding-evaluation/EXPECTED_CLUES.md)、[复现](noise-folding-evaluation/REPRODUCE.md)。同目录保存分析脚本和结果。 / Standalone noise-folding experiment with rules, counterexamples, preregistration, reproduction, scripts, and results. |

## 外部案例与归因 / External cases and attribution

这些是限定环境下的案例记录和分析者判断，不是普遍成功率或根因证明。历史资料中的外部 issue 状态不代表今天的状态；不因整理而重新采集或更新其结论。

These are observations in bounded environments and analyst judgments, not general success rates or proof of causes. Historical issue status is not current status; organization alone does not justify recollecting evidence or revising conclusions.

| 目录或入口 / Directory or entry | 内容 / Contents |
|---|---|
| [external-alpha/](external-alpha/) · [README](external-alpha/README.md) | 第一轮 alpha 总览、来源登记、保护清单、重放结果和验证/复现脚本。另见[检查](external-alpha/CHECKS.md)与[复现说明](external-alpha/REPRODUCE.md)。 / First alpha overview, provenance, preservation manifest, replay results, and verification/reproduction tools. |
| [external-alpha/pybind11-3445/](external-alpha/pybind11-3445/) · [CASE](external-alpha/pybind11-3445/CASE.md) | CMake 包发现案例的预登记、结果和摘录。 / Preregistration, results, and excerpts for CMake package discovery. |
| [external-alpha/spades-1339/](external-alpha/spades-1339/) · [CASE](external-alpha/spades-1339/CASE.md) | 含空格路径案例、两次补充尝试、结果和摘录。 / Paths-with-spaces case, follow-up attempts, results, and excerpts. |
| [external-alpha/meson-16136/](external-alpha/meson-16136/) · [CASE](external-alpha/meson-16136/CASE.md) | Meson/Cargo 依赖案例的预登记、结果和摘录。 / Preregistration, results, and excerpts for the Meson/Cargo dependency case. |
| [external-alpha/attribution/](external-alpha/attribution/) · [README](external-alpha/attribution/README.md) | 第一轮证据归因、探针、原材料摘要和校验器。 / First-round attribution, probes, original-material hashes, and verifier. |
| [external-alpha/clone-fs-attribution/](external-alpha/clone-fs-attribution/) · [README](external-alpha/clone-fs-attribution/README.md) | 共享 CWD 的历史建模调研与同步探针；旧实现分析不覆盖当前 FsContext 模型。 / Historical shared-CWD investigation and synchronized probes; its old implementation analysis does not override the current FsContext model. |
| [external-alpha/batch-2/](external-alpha/batch-2/) · [README](external-alpha/batch-2/README.md) | 第二轮[筛选](external-alpha/batch-2/SCREENING.md)、[检查](external-alpha/batch-2/CHECKS.md)、来源和复现脚本，含 [pip-12996](external-alpha/batch-2/pip-12996/) 与 [bracket-lib-376](external-alpha/batch-2/bracket-lib-376/)。 / Second-round screening, checks, provenance, and reproduction, including the pip and bracket-lib cases. |
| [external-cases/pybind11-5626/](external-cases/pybind11-5626/) · [README](external-cases/pybind11-5626/README.md) | 冻结外部诊断案例；原 issue 未复现。含[冻结说明](external-cases/pybind11-5626/FROZEN.md)、[报告](external-cases/pybind11-5626/REPORT.md)、[噪声分析](external-cases/pybind11-5626/NOISE_ANALYSIS.md)、CSV 台账及校验脚本。 / Frozen diagnostic case whose original issue was not reproduced, with reports, noise analysis, ledgers, and verifiers. |

## 性能研究 / Performance research

| 目录或入口 / Directory or entry | 内容与边界 / Contents and boundary |
|---|---|
| [performance/a3-completion-check/](performance/a3-completion-check/) · [README](performance/a3-completion-check/README.md) | 完成检查的历史基准：[计划](performance/a3-completion-check/PLAN.md)、[测量](performance/a3-completion-check/MEASUREMENTS.md)、[设计](performance/a3-completion-check/DESIGN.md)、[优化验收](performance/a3-completion-check/OPTIMIZATION.md)。配套 JSON、测量/校验脚本和 [optimization/](performance/a3-completion-check/optimization/) 结果保留原位。 / Historical completion-check plans, measurements, design, optimization validation, and supporting data/scripts. |
| [performance/rf600-streaming/](performance/rf600-streaming/) · [README](performance/rf600-streaming/README.md) | 同源码 parser 输入适配器的流式基准；合成输入的 RSS 变化不能外推为端到端 record 收益。 / Same-source streaming parser-adapter benchmark; synthetic-input RSS changes do not establish end-to-end recording gains. |

## 发布与打包 / Publication and packaging

| 入口 / Entry | 用途与边界 / Purpose and boundary |
|---|---|
| [CRATE_README.md](CRATE_README.md) | Cargo 包使用的用户说明；与根 README 的用途不同。 / User documentation selected for the Cargo package, distinct from the development README. |
| [ALPHA_READINESS.md](ALPHA_READINESS.md) | 2026-09-20 发布准备、MIT 元数据与打包审查快照。 / Dated alpha readiness, MIT metadata, and packaging review. |
| [packaging/PACKAGE_POLICY.md](packaging/PACKAGE_POLICY.md) | 源码包允许清单与安全边界，旧文件数/状态按日期解读。 / Source-package allowlist and boundaries; file counts and status are dated. |
| [packaging/BUILD_PATHS.md](packaging/BUILD_PATHS.md) | release 二进制中构建路径的归因和处理设计。 / Attribution and treatment design for build paths embedded in release binaries. |
| [packaging/check_package.py](packaging/check_package.py) | 源码归档检查器；不能代替 Git 历史或二进制的发布审查。 / Source-archive checker; not a substitute for Git-history or binary publication review. |

本地开发历史、这份索引和内部材料的整理不构成公开发布授权。公开仓库导出仍需独立干净目录、显式允许清单和隐私审查；crate 打包通过也不代表整个仓库、raw 日志或二进制可直接分享。

Organizing the private development tree, this index, or internal material does not authorize publication. Public exports still require an isolated clean tree, an explicit allowlist, and privacy review. A passing crate-package check does not establish that repository history, raw traces, or binaries are ready to share.
