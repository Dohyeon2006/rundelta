> 本地证据路径说明：本文及本索引所链接历史文档中的 `target/`、本机实验目录和未发布原始日志路径，仅用于说明原本地复现实验位置；这些产物不随公开版提供，不是可访问的公开证据链接。公开可核验材料以仓库实际收录的文件和公开派生合同为准，不能据本地路径推断已发布原始记录。
> Local evidence paths: `target/`, local experiment directories and unpublished logs describe original local reproductions. Their artifacts are not shipped with this public edition and are not accessible public evidence links.

> 公开树说明（2026-10-16）：本树引用的 ACCEPTANCE-before-reliability、FIELD_CASES、HOLDOUT_RESULTS、RANKING_REPAIR 及对应结果 JSON 为脱敏派生副本；原始证据保留在私有归档，未重新实测。下文关于私有树、历史路径或发布阻塞的结论属于当时状态，不代表本公开副本仍含这些路径。公开派生合同见 [REGRESSION_SET_PUBLIC.json](../REGRESSION_SET_PUBLIC.json)；原 v1 原件校验仍属私有树。
> Public references point to redacted historical derivatives, not fresh measurements. Original v1 evidence verification remains private; the public archive has a separate integrity contract.

# MIT 与安全源码打包

## 2026-10-09 归档元数据与规范化 manifest 校验

独立检查器现在拒绝非空 tar owner/group/link 名称、非零 uid/gid/设备号，
以及成员和全局 PAX 扩展。正文扫描不覆盖这些独立元数据；本轮实际 Cargo
1.98.1 生成的 19 文件包使用空名称、零编号与无 PAX 的普通文件 header，仍被接受。
新增负例直接构造这些字段的归档字节，并在普通 Python、`-O` 和
`PYTHONOPTIMIZE=1` 下核对拒绝结论。

仅检查解码后的 PAX 字典会漏掉空扩展 header；检查器还要求连续的普通文件
物理 header，拒绝包括空 PAX 在内的隐藏扩展。实际 Cargo 的 GNU header 与
合成 USTAR 控制使用短 ASCII 名称、零 string/reserved padding 和规范 octal
数字 padding；NUL 后的隐藏字节、成员正文后的非零 padding 及 tar 尾随数据
均被拒绝。gzip 仅接受无可选文件名或精确的
`RunDelta-0.2.0-alpha.1.crate` 文件名；其他可选字段、reserved flags、截断、
拼接流和尾随 bytes 均被拒绝。保留无压缩 tar 的既有内存控制入口。
gzip 完整性验证丢弃分块输出，tar 按成员读取；不先把整个展开包放入内存，
也不增加总量预算、临时文件写入或依赖。

`Cargo.toml.orig` 与源码的原字节核对继续保留。规范化 `Cargo.toml` 的完整
TOML 语义必须等于原 manifest 加本包实测的 Cargo 转换：六个 build/自动 target
开关为 false，简写依赖版本展开为 version table。binary 名称和路径、所有
依赖与 feature、构建配置及其他字段不能在规范化文件中独立变化；false 与
数字 0 也不视为相同。当前支持单一 `rundelta` binary、package/bin/dependencies
三个源码 section；未来新增构建脚本、target 或不同 Cargo 转换需显式复审。
检查采用 TOML 数据比较，不依赖字段顺序或注释。

这次没有增加包文件、扫描例外、生产依赖或公开发布内容；200,000-byte
单成员限制、MIT、版本与 allowlist 保持。精简包生产构建通过；包内 `cargo test`
仍因已声明排除的冻结 fixture 和私人开发测试模块而不受支持，完整测试继续在
开发 checkout 运行。

The checker rejects nonempty tar owner/group/link names, nonzero numeric owner
or device fields, and member/global PAX metadata. The actual Cargo 1.98.1 archive
with anonymous ordinary-file headers remains accepted. Negative archives are
checked under ordinary Python, `-O`, and `PYTHONOPTIMIZE=1`.

Physical-header checks reject even empty hidden extensions and nonzero tar
padding, including bytes discarded after NUL by decoded string/numeric fields.
The observed Cargo GNU and synthetic USTAR headers remain accepted. Gzip allows
only the exact package filename or no filename, with no other optional metadata,
reserved flags, truncation, concatenation or trailing bytes. Raw tar controls
remain supported. These finite format checks do not prove the absence of every
secret encoding or authenticate an archive's origin.
Carrier validation discards bounded chunks and tar files are read per member;
the complete expanded archive is never buffered first. No total-size budget,
temporary-file I/O or dependency is added.

The complete normalized manifest must match the original TOML semantics plus
the observed package normalization: six false build/automatic-target defaults
and expansion of version-only dependencies. Binary targets, dependencies,
features and build settings cannot change independently; scalar types remain
distinct. Future source sections, targets or Cargo normalization need review.
Exact source-byte checks, the file allowlist, privacy exceptions, MIT contract,
version and member budget remain unchanged. This local validation does not
authorize publication.

> 历史状态说明（2026-09-21）：本文保留 2026-09-20 的打包基线、旧测试数量与当时的
> README 合并状态。当前允许清单按下文有日期的审查显式扩展，安全边界不放宽；实际重构状态与最新验证结果见
> `../REFACTOR_STATUS.md`，不要把下文“尚未应用”当作当前 README 状态。

日期：2026-09-20。版本保持 0.1.0；没有发布、tag、推送或改写历史。此策略只限定 Cargo 归档，不等于整个 Git 仓库或 release 二进制已脱敏。

## 策略

根 LICENSE 使用标准 MIT 正文，版权行为 `Copyright (c) 2026 The RunDelta contributors`。Cargo metadata 设置 license、克制的 description、readme、五个关键词和两个已核对的分类。没有填写未经验证的 repository/homepage/documentation，也没有虚构 MSRV。Cargo 对 URL 未设置的提示保留。

采用逐文件 `include` 白名单，而不是依赖 Git 忽略文件或目录黑名单。允许 Cargo 文件、MIT 文本、下列明确列出的 Rust 模块文件与 [crate 用户说明](../CRATE_README.md)。根 README 原修改不动，也不进入包。新增的打包检查器、报告和 README 合并建议本身同样不进入包。

Cargo 的生成/规范化文件不能忽略：`Cargo.toml.orig`、规范化 Cargo.toml 和 Cargo.lock 实际存在；从干净 Git 提交打包还会有 `.cargo_vcs_info.json`，只接受干净状态、根包路径和提交 SHA。本轮实际核查 tar 内容，不以 cargo package 成功或 gitignore 作为隐私证明。[Cargo 清单规则](https://doc.rust-lang.org/cargo/reference/manifest.html#the-exclude-and-include-fields)。

## 允许的完整包清单

```text
.cargo_vcs_info.json    # 仅 Git 来源包；无 Git 的干净源码导出包没有此项
Cargo.lock
Cargo.toml
Cargo.toml.orig
LICENSE
docs/CRATE_README.md
src/capture/completion.rs
src/capture/mod.rs
src/capture/platform_exit.rs
src/capture/policy.rs
src/capture/reduction.rs
src/capture/supervision.rs
src/cli.rs
src/diff/mod.rs
src/main.rs
src/model/mod.rs
src/parse/fs_tests.rs
src/parse/mod.rs
src/report/mod.rs
src/store/deletion.rs
src/store/mod.rs
```

没有 IDE、target、外部案例、性能材料、内部验收报告、探针、冻结 fixture、测试生成物或完整追踪日志。未来新增源文件须显式审查并加入清单，否则不会自动进入包。

`src/parse/fs_tests.rs` 作为源码模块保留；它不含外部采集记录或独立探针。现有 Rust 源码中的少数公开合成路径与临时目录排序常量并不是本机路径数据，未删除或改写。检查器仅对 `src/report/mod.rs` 中四种精确引号包围的拼写作已审查常量处理；不允许它们的后缀变体、不允许在 README 等其他文件使用此例外。运行时采集到的真实用户路径不参与这种打包检查，也没有被产品过滤。

## 实际验收

以 HEAD 的独立源码导出加本轮预定 Cargo/LICENSE/用户说明构成干净副本，未导入原工作树 README/IDE/probe 修改；新的 Cargo 缓存和 target 均在私人临时工作根。

| 检查 | 结果 |
|---|---|
| `cargo build --release` | 退出 0 |
| `cargo package --list --locked --offline` | 退出 0；无 Git 来源时 14 文件 |
| `cargo package --locked --offline` | 退出 0；14 文件约 130.2 KiB，成功完成包内构建验证 |
| 归档检查与字节核对 | MIT/metadata 正确；真实归档与允许源码逐字相同；普通、hex、多层转义路径检查通过 |
| 新检查器负例 | 18 项全部拒绝；正常归档与精确公开常量通过 |
| Git 文件选择探针 | 在独立 Git 副本加入 IDE、target、额外 Rust 文件、外部材料和测试输出五个哨兵文件；`--list --allow-dirty` 仍仅列出 15 个允许项，未接受脏归档 |
| 开发源码树 Rust 单元 | 43/43 通过，未改测试断言或 fixture |
| 开发副本 CLI | 43 次调用通过，包含最小工作流、0/1/2/3 与兼容/损坏夹具 |
| 实际归档解包后 release 构建 | 退出 0 |
| 解包后 release CLI | record、diff 0/1/2/3、list/show 文本和 JSON，共 16 次调用通过 |

初版新增扫描器把现有 Rust 字符串中的转义引号常量误拒为路径；仅修正精确引号匹配后通过。没有扩大目录白名单，也没有删除原有产品测试以获得通过。

**包内测试限制已实测：** 对精简归档运行 `cargo test --no-run` 返回 101，原因是被明确排除的 `tests/fixtures/trace.txt` 不存在。Cargo 打包也提示不纳入 acceptance 集成目标。构建、安装和运行不依赖该 fixture；完整开发树 43 个单元仍通过。crate 用户说明已公开这一边界。本轮不把冻结数据放回包、不删测试、不增加跳过断言的功能；完整回归继续在开发树运行。

最后应从本轮实际干净提交复验 15 文件 Git 包；生成的 VCS SHA 必须等于提交 ID。该额外自动文件不允许携带 dirty 标记或本机工作根。

## 复算

在独立副本中构建，`SOURCE`、`LAB`、`ARCHIVE` 均由操作者设置，`LAB` 不得是原仓库。后端 strace 另行提供，不随 crate 分发：

```bash
cd "$SOURCE"
env CARGO_HOME="$LAB/cargo-home" CARGO_TARGET_DIR="$LAB/build" cargo build --release
env CARGO_HOME="$LAB/cargo-home" CARGO_TARGET_DIR="$LAB/build" cargo package --list --locked --offline
env CARGO_HOME="$LAB/cargo-home" CARGO_TARGET_DIR="$LAB/build" cargo package --locked --offline
python3 -B docs/packaging/check_package.py "$ARCHIVE" --source "$SOURCE" \
  --private-prefix "$SOURCE" --private-prefix "$LAB" --self-test
```

检查器在开发仓库运行，**不随 crate 打包**；`--source` 比较用户文档/源码原字节，路径扫描还能覆盖指定的非标准私人根。固定清单拒绝 IDE、探针、外部资料等额外成员，支持层叠 JSON/C/Unicode/URL 和裸 hex 检查。不把有限模式检查说成对所有秘密编码的证明。

旧 external-alpha 与冻结检查器未修改。原工作树 probe 的既有换行差异仍会使旧材料哈希检查失败；已提交旧材料的快照用于验证历史证据，不通过改哈希解决此差异。

## 尚未解决的发布层面

- **公开 Git / 历史：** 当前树仍有至少六份含真实用户根路径的历史材料：ACCEPTANCE-before-reliability.md、FIELD_CASES.md、FIELD_CASES_RESULTS.json、HOLDOUT_RESULTS.json、RANKING_REPAIR.md、RANKING_REPAIR_RESULTS.json（均在 docs 下）。本轮开始时只读查询的 10 个可达提交全部命中这个已知用户根；这不是全面的秘密扫描。Cargo include 不删除它们，也不从历史对象中消除数据。
- **二进制：** 默认 release 中的 52 个构建路径仍存在；归因及最小处理设计见 [BUILD_PATHS.md](BUILD_PATHS.md)。未改构建配置，不能据本轮包校验发布默认二进制。
- **根 README：** 已先检查并保留用户 diff；内部 README 合并建议（未随公开版发布） 尚未应用。crate 使用独立准确的用户说明，不代表仓库首页已更新。
- **许可边界：** MIT 授权覆盖 RunDelta，不取代依赖或 strace 自身许可；尚未完成二进制分发的全部依赖/工具链通知整理。

首次公开推送前，仍需用户明确选择并授权公开范围：仅导出经审查的源码/文档新发布树，或保留既有历史并对指定文件、分支、tag 与所有相关历史对象做专门脱敏重写。后一选项会改变提交 ID，可能涉及冻结引用的迁移方案，必须保留私有不可变基线，不可静默更新旧哈希。若任何远程已含相关对象，还须单独授权远程协作/替换范围。本轮两种方案均未执行，现有历史、refs、冻结材料和校验器均保留。

## 2026-10-07 校验器入口加固

独立 checker 的外部包输入检查使用显式 `require`，普通 Python、`-O` 和
`PYTHONOPTIMIZE=1` 对相同合法包/非法 inventory、隐私与 license 输入保持相同
接受结论。可选 self-test 增加重复、特殊、越界与大成员反例；200,000 byte 单成员
预算、精确 allowlist、privacy exceptions 与 MIT 合同未放宽。纯 metadata 合同单测
随校验职责移动到既有 model 文件，避免新增 package 文件或扩大 scanner 例外。

External package checks remain active under Python optimization. The exact file
allowlist, privacy exceptions, MIT contract and 200,000-byte member budget remain
unchanged. Full gate validates a fixed source snapshot and retains package logs;
this local validation does not authorize publication.

## 2026-10-07 capture 第一批机械提取白名单审查

用户授权分三批拆分 capture，并要求第一批完成固定源码验收后才能进入第二批。
第一批仅新增生产源文件 `src/capture/completion.rs`，承接原 capture 的目标完成
选择、同代退出选择、provenance 投影与相关合同测试；完成事实归约与 outcome
译码仍由父 capture 模块承担。依赖保持 parse/model，不添加 I/O、生产依赖或反向耦合。
Cargo 的逐文件 include 与独立 checker 的 SOURCES 仅增加此精确路径，未使用
目录 glob。候选闭包同样强制该文件存在、为普通文件且与工作树源码字节相同。

The user authorized three capture extraction batches, with fixed-source
acceptance of batch one required before batch two. Batch one adds only the
production source `src/capture/completion.rs`, moving existing target-completion
selection, same-generation exits, provenance projection and related contract
tests. Completion reduction and outcome decoding remain in the capture parent.
Dependencies remain parse/model,
with no new I/O, production dependency or reverse coupling. Cargo's file-by-file
include and the independent checker's SOURCES add only that exact path, without
a directory glob. The candidate closure also requires this regular file and
exact equality with its working-tree source bytes.

本次是源文件布局的显式审查，不是公开仓导出或历史发布。机械提取来源、最终源码
与 package 构建/原字节/隐私验证必须由本批自己的固定候选凭据证明，不借用旧
14/15 文件包的历史 PASS。MIT、精确 inventory、原文件字节核对、200,000-byte
单成员限制和所有解码隐私检查保持；未新增 scanner 例外。内部报告、原始证据、
探针、冻结 fixtures、target、IDE 与私人开发测试模块继续排除，冻结五例不变。
后续批次的新增文件须各自再经同样的显式白名单审查，不能提前纳入第一批。

This reviews source layout, not public-repository export or publication of private
history. Mechanical extraction provenance, final source and package build,
byte-equality and privacy results require this batch's fixed-candidate receipts;
historical 14/15-file PASS results do not verify this payload. MIT, exact inventory,
original-source byte checks, the 200,000-byte member cap and every decoded privacy
check remain in force, with no new scanner exception. Internal reports, raw
evidence, probes, frozen fixtures, target, IDE files and private developer test
modules remain excluded, and the frozen five cases remain unchanged. Later
batches require their own explicit file review and are not admitted early.

## 2026-10-07 capture 第二批事实归约与退出译码白名单审查

第二批以前一批已验收的源码为基线，仅新增生产源文件
`src/capture/reduction.rs` 与 `src/capture/platform_exit.rs`。reduction 承接原
capture 的独立完成事实、资源限制事实、纯归约与 metadata 投影，依赖 model/std；
platform_exit 承接原有的历史 outcome 与实际进程退出状态译码，依赖
reduction/model/std 与既有 libc。译码适配依赖纯事实类型，纯归约不反向依赖
Linux 进程或信号监督；本批不添加 I/O、生产依赖或改变持久化与退出码合同。
相关合同测试随对应职责迁移，冻结五例保持不变。

Batch two uses the accepted batch-one source as its baseline and adds only
the production sources `src/capture/reduction.rs` and
`src/capture/platform_exit.rs`. Reduction receives the existing independent
completion facts, resource-limit facts, pure reduction and metadata projection,
depending on model/std. Platform exit receives the existing historical-outcome
and process-exit-status decoding, depending on reduction/model/std and the
existing libc dependency. The decoding adapter depends on pure fact types;
pure reduction does not depend back on Linux process or signal supervision.
This batch adds no I/O or production dependency and preserves persistence and
exit-code contracts. Related contract tests move with their responsibilities;
the frozen five cases remain unchanged.

Cargo include、独立 checker 的 SOURCES 与候选闭包的 NEW_RELEASE_ASSETS
各自仅增加上述两个精确路径。每个新源文件必须为普通文件、与固定候选和工作树
原字节一致，并通过既有完整解码隐私扫描与单成员体积门禁。第一批审查段落和
14/15 文件包历史记录保留；旧 PASS 不能证明本批新增载荷。本批实际源码、
机械来源、package 与隐私结果由本批独立凭据绑定，不代表公开仓导出或发布授权。
未使用目录 glob、未新增 scanner 例外，MIT、200,000-byte 单成员限制、
原字节核对和排除私人材料的边界不变。第三批文件仍未加入清单。

Cargo include, the independent checker's SOURCES and the candidate closure's
NEW_RELEASE_ASSETS each add only those two exact paths. Each source must be a
regular file, match the fixed candidate and working-tree bytes, and pass the
existing decoded privacy checks and member-size gate. Batch-one review and the
historical 14/15-file package results remain intact; their PASS results do not
verify this added payload. This batch's source, extraction provenance, package
and privacy results require its own bound receipts and do not authorize public
export or publication. No directory glob or scanner exception is added. MIT,
the 200,000-byte member cap, original-byte checks and private-material exclusion
remain unchanged. Batch-three files are not admitted by this review.

## 2026-10-07 capture 第三批监督与资源配置白名单审查

第三批以前两批各自已验收的固定源码为前提，仅新增生产源文件
`src/capture/supervision.rs` 与 `src/capture/policy.rs`。supervision 承接原有
Linux 子进程、pidfd、信号与后端预检监督；policy 承接共享资源配置。监督依赖
policy/reduction/platform_exit 与 std/libc/anyhow，通过调用者提供的 anchored
trace 长度读取函数取得存储观察，生产路径不直接依赖 store 或父 workflow
内部；cfg(test) 随迁的既有存储锁集成测试仍使用 store。父 capture 保留解析、归约、存储与
freeze→完整提交→显式恢复的编排。owner 字段保持私有，无新增生产依赖。
实际职责、依赖、机械来源和兼容行为仍须由本批固定候选的审查与验收凭据证明。

Batch three requires separate fixed-source acceptance of both preceding
batches and adds only the production sources `src/capture/supervision.rs` and
`src/capture/policy.rs`. Supervision receives the existing Linux child, pidfd,
signal and backend-preflight supervision; policy receives shared resource
configuration. Supervision depends on policy/reduction/platform_exit and
std/libc/anyhow and obtains anchored trace-length observations through a
caller-supplied function. Its production path does not depend directly on store
or parent-workflow internals; the migrated existing storage-lock integration
test still uses store under cfg(test). The capture
parent retains parsing, reduction, persistence and the freeze→complete commit→
explicit restoration workflow. Owner fields remain private and no production
dependency is added. This batch's fixed-candidate review and acceptance receipts
must establish the actual scope, dependencies, extraction and compatibility.

Cargo include 与独立 checker SOURCES 仅增加上述两个精确 Rust 路径。
私有验收闭包另外要求 `tests/probes/record_restore_failure.c`：该 LD_PRELOAD
探针只在指定 recorder 成功发布 finalization marker 后一次注入恢复失败，
覆盖两种保存合同的正常恢复、action 恢复失败与实际 sigprocmask mask 恢复失败。
它保存提交时原字节、检查 Drop 重试后的完整原信号状态，不添加生产环境开关。
C 探针、Python 回归与内部验收凭据不进入 Cargo 包，也不自动获得公开导出授权。
probe 同时区分 pthread_sigmask 的返回值合同，但未被调用的 API 不算已验收。
完整 action 比较的定义域是 probe 经真实 libc 设置后重新查询的 fixture 初态，
按 handler、完整 flags 和有效信号 mask 成员比较；不 mask 未知 flags、不比较
struct padding。raw exec flags 与 libc 安装后 SA_RESTORER 的既有 ABI 差异另行
原样记录，不把它解释成所有 kernel action bits 都能逐位恢复。

Cargo include and the independent checker's SOURCES add only those two exact
Rust paths. The private acceptance closure additionally requires
`tests/probes/record_restore_failure.c`: this LD_PRELOAD probe injects a one-shot
restoration error only after the named recorder successfully publishes its
finalization marker. It covers normal restoration, action restoration failure
and failure of the actual sigprocmask mask-restoration path under both saved
contracts. It preserves committed bytes and checks complete original signal
state after Drop's retry, without a production environment switch. The C probe,
Python regressions and internal receipts remain outside the Cargo package and
receive no automatic public-export authorization. The probe also distinguishes
pthread_sigmask's return-value contract, but an uncalled API is not validated.
Full action comparison covers the fixture state established and queried through
real libc, comparing handler, complete flags and valid signal-mask members.
It masks no unknown flags and compares no struct padding. The existing ABI
difference between raw exec flags and libc's installed SA_RESTORER is retained
separately; this does not claim bitwise restoration of every kernel action bit.

第一、二批与旧包结果保留为各自阶段历史，不代替本批验证。MIT、精确 inventory、
原字节核对、200,000-byte 单成员限制和全部隐私解码扫描保持，不新增 scanner
例外、不扩大目录范围、不修改冻结五例或其 fixtures。当前完整包清单不含私有
C 探针；候选闭包须证明 Rust 新文件和私有探针均存在、为普通文件并与冻结候选
原字节一致。实际归档、构建与完整门禁完成前，本段只记录批准的边界与验收要求。

Batch-one, batch-two and historical package results remain evidence of their
own stages and do not replace this validation. MIT, exact inventory,
original-byte checks, the 200,000-byte member cap and all decoded privacy checks
remain unchanged, with no scanner exception, directory expansion or frozen-case
fixture change. The current complete package inventory excludes the private C
probe. The candidate closure must establish that both new Rust files and the
private probe are regular files matching the frozen candidate bytes. Until the
actual archive, build and full gate finish, this section records the authorized
boundaries and acceptance requirements only.

## 2026-10-12 删除隔离协议精确白名单审查

本轮只为新私有存储职责增加 `src/store/deletion.rs`：store 子模块持有隔离目录
描述符、v1 删除收据和显式 list/restore/purge，不反向依赖 capture/model 的
事实归约、diff 或 report，不新增生产依赖。Cargo include、独立 checker SOURCES
和候选资产闭包逐条加入该精确普通文件；合法源码包从原 19 个基础成员变为 20
个基础成员（干净 Git 的 VCS 自动成员另按既有规则），不是目录 glob 或任意成员
豁免。200,000-byte 单成员上限、归档规则、所有隐私解码与扫描例外保持原值。
新增 `tests/probes/deletion_protocol.c` 仅测试已拥有的独立夹具，不进入包；私有
收据工具、日志、原始证据、fixture 和 target 继续排除。该说明只批准精确范围，
不以旧 package PASS 代替本轮实际归档、字节、隐私与构建核验，不授权发布。

The reviewed addition is exactly `src/store/deletion.rs`, a private store child
owning anchored isolation, v1 deletion receipts and explicit recovery. It adds no
production dependency or reverse coupling to capture facts, diff or report.
Cargo include, checker SOURCES and candidate closure name this regular file;
20 base members replace the old 19-member source inventory, with the existing
clean-Git VCS rule unchanged. This changes the exact reviewed inventory, not the
200,000-byte member budget, archive validation, privacy decoding or scanner
exceptions. `tests/probes/deletion_protocol.c`, private tools/evidence, fixtures
and target remain excluded. Actual package acceptance is still required for
this batch; old passes and this scope review do not authorize publication.

## 2026-10-12 同 PID 采集所有者精确白名单审查

本批唯一新增生产文件为 `src/capture/owner.rs`，由 volatile capture adapter 持有密封原生请求、当前 inode exec 和 environment guard，不依赖 diff/report，不让 model 依赖 OS。Cargo include 与 checker SOURCES 加入该精确普通文件；基于删除隔离批的 20 个基础成员，现在为 21 个基础成员，Git VCS 自动成员沿用原规则。没有新生产依赖、锁文件改动、目录 glob、预算或隐私扫描例外。`tests/probes/capture_syscall.c`、`tests/probes/capture_host_hooks.c`、`tests/probes/capture_request_fault.c`、`tests/probes/capture_chld_target.c`、`tests/probes/capture_caught_control.c` 与 `tests/capture_owner_support.py` 属独立测试探针，不进入生产包。源码迁移与实际包/字节/隐私检查仍须按本批执行，本文不冒充验收通过或授权发布。

The only added production member is exactly `src/capture/owner.rs`. The volatile capture adapter owns sealed native input, current-inode exec and the environment guard; model remains independent of OS and no diff/report dependency is added. Cargo include and checker SOURCES name this regular file: 21 base members replace deletion batch's 20, preserving the clean-Git VCS rule. No production dependency, lock change, glob, budget or privacy exception is added. `tests/probes/capture_syscall.c`, `tests/probes/capture_host_hooks.c`, `tests/probes/capture_request_fault.c`, `tests/probes/capture_chld_target.c`, `tests/probes/capture_caught_control.c` and `tests/capture_owner_support.py` are test-only and excluded. Actual package/byte/privacy checks and source migration evidence remain required; this inventory review is not a validation result or publication permission.
