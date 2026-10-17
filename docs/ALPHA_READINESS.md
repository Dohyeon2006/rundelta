> 本地证据路径说明：本文及本索引所链接历史文档中的 `target/`、本机实验目录和未发布原始日志路径，仅用于说明原本地复现实验位置；这些产物不随公开版提供，不是可访问的公开证据链接。公开可核验材料以仓库实际收录的文件和公开派生合同为准，不能据本地路径推断已发布原始记录。
> Local evidence paths: `target/`, local experiment directories and unpublished logs describe original local reproductions. Their artifacts are not shipped with this public edition and are not accessible public evidence links.

> 公开树说明（2026-10-16）：本树引用的 ACCEPTANCE-before-reliability、FIELD_CASES、HOLDOUT_RESULTS、RANKING_REPAIR 及对应结果 JSON 为脱敏派生副本；原始证据保留在私有归档，未重新实测。下文关于私有树、历史路径或发布阻塞的结论属于当时状态，不代表本公开副本仍含这些路径。公开派生合同见 [REGRESSION_SET_PUBLIC.json](REGRESSION_SET_PUBLIC.json)；原 v1 原件校验仍属私有树。
> Public references point to redacted historical derivatives, not fresh measurements. Original v1 evidence verification remains private; the public archive has a separate integrity contract.

# 探索性 alpha 发布准备审查与 MIT 打包更新

> 历史状态说明（2026-09-21）：本文冻结 2026-09-20 的发布准备审查；根 README
> 已在后续全量重构中独立更新。下文“尚待合并”等措辞只描述当日状态，当前实现与验收见
> 根 README 和 `REFACTOR_STATUS.md`。

## 当前状态：MIT 元数据与安全打包（2026-09-20）

本节更新前一轮审查的处理状态；下方原始审查保留当时事实，不应将其中“尚无 LICENSE”“包有 161 文件”等当作当前 crate 状态。

**已解决源码 crate 层面的许可元数据与文件隔离。仍不能据此直接首次公开推送整个 Git 历史或分发默认 release 二进制。** 根 README 的现有修改未动，仓库首页合并仍待处理。

| 问题 | 当前状态 | 证据/边界 |
|---|---|---|
| P1-A 项目 MIT 与 package metadata | 已解决本轮要求 | 标准根 LICENSE、指定 contributors 版权；license/description/readme/keywords/categories 已设置；版本仍 0.1.0。未知 URL 留空，不添加未经验证的 MSRV |
| P1-B 源码 crate 包含内部/私人材料 | 已解决打包层面 | 逐文件 include；无 Git 来源的实际包 14 文件，干净 Git 来源按 Cargo 规则多一个 VCS 文件；真实归档正文、hex/转义路径、许可证和源码字节检查通过 |
| P1-B 整个 Git 仓库及历史 | **仍阻塞公开 Git 推送** | 至少六份旧材料包含真实用户根；开始审查时 10 个可达提交全部命中已知根。include 不改变 Git 对象；本轮未清理或重写 |
| P1-B 默认 release 构建路径 | **仍待二进制发布阶段处理** | 52 个路径均在 .rodata：依赖 32、标准库 20；228 个符合 panic 位置布局的引用。strip 不消除。只做独立探针与设计，未改构建配置 |
| P1-C crate 用户说明 | 已提供独立准确说明 | Cargo 显式使用 docs/CRATE_README.md，包含安装/卸载、数据与隐私、退出码、CWD/FsContext 与兼容限制，不包含内部资料链接 |
| P1-C 根 README | **尚待合并** | 原 diff 完整保留；当时的合并建议属于未发布内部资料；本公开副本不提供该资料 |

本轮实测：干净源码 `cargo build --release`、`cargo package --list`、完整 package 构建验证均退出 0；开发树 43 个 Rust 单元通过；43 次开发副本 CLI 验证通过；归档解包后 release 构建以及 16 次 record/diff/list/show 文本/JSON 调用通过。diff 的 0/1/2/3 语义未变。新增归档检查器的 18 个负向样例全部拒绝。

包不携带开发回归 fixture，因此包内 `cargo test --no-run` 已实测因缺少 fixture 退出 101；这项边界已写入 crate 用户说明。没有删除测试断言或把冻结素材重新放回包。它是安装/构建源码分发，不是完整开发验收分发。

详见 [实际清单与复算](packaging/PACKAGE_POLICY.md)、[构建路径归因](packaging/BUILD_PATHS.md)、内部 README 合并建议（未随公开版发布）。新检查器独立存在，没有修改旧 external-alpha 或冻结材料校验器。原工作树 probe 的既有换行差异仍保留；旧材料检查在对应已提交快照中运行，不以改哈希消除差异。

### 首次公开推送前必须另行授权的范围

仍含已知用户根的当前文件：`docs/ACCEPTANCE-before-reliability.md`、`docs/FIELD_CASES.md`、`docs/FIELD_CASES_RESULTS.json`、`docs/HOLDOUT_RESULTS.json`、`docs/RANKING_REPAIR.md`、`docs/RANKING_REPAIR_RESULTS.json`。这是已确认的最小集合，不是所有可能秘密的完整清单。

用户需明确选择并授权：

- 只建立经审查的公开源码/文档导出树，保留本仓库与冻结证据为私有不可变基线；公开树包含哪些资料、是否需要新的公开证据副本另定。
- 或保留项目历史，但为上述路径及进一步扫描发现的数据制定逐文件、逐 ref 的历史脱敏方案；明确分支/tag 范围、提交 ID 变化、冻结引用迁移与私有归档保留，不能静默替换旧 fixture/哈希。
- 如涉及任何既有远程历史替换或首次公开推送，还须明确远程、分支、内容和操作范围。本轮没有推送、force push、tag、发布、reset、clean、checkout 或历史改写。

建议继续准备**源码形式、Linux/外部 strace、观察线索而非根因保证**的探索性 alpha。安全 crate 是已完成的一层，公开 Git、默认二进制和根 README 仍有独立门禁。

## 先前审查留档（下文基于 da0d662）

日期：2026-09-20。审查基准：`da0d6626a99231ca32df23e93f5e24ca65609ef3`。本轮仅提交第二轮外部验证材料，随后只读审查产品并新增本报告；没有更改产品、README、版本、tag、发布配置或远程状态。

**结论：可以继续准备限定范围的探索性 alpha，但当前提交和默认产物不宜直接公开发布。** 干净目录 release 构建、CLI 工作流和退出码验证通过；发布前有三项 P1 门禁：明确项目许可证/发布元数据、隔离发布材料中的本机信息、更新过期且缺少安装卸载说明的发布入口文档。这些是发布准备问题，不是本轮确认的新采集或解析缺陷。

## 1. 提交与工作区保护

本轮唯一提交：

```text
da0d6626a99231ca32df23e93f5e24ca65609ef3
Add second external alpha validation batch
```

仅含 `docs/external-alpha/batch-2/` 的 17 个文件，2,292 行新增：

```text
CHECKS.md
README.md
SCREENING.md
analyze.py
bracket-lib-376/CASE.md
bracket-lib-376/RESULTS.md
bracket-lib-376/evidence.json
bracket-lib-376/summary.json
original-materials.json
pip-12996/CASE.md
pip-12996/RESULTS.md
pip-12996/evidence.json
pip-12996/summary.json
provenance.json
reproduce.py
test_redaction.py
verify.py
```

提交前扫描确认没有第三方源码副本、虚拟环境、完整 raw、构建产物、私人绝对路径、凭据形状或 IDE 文件；batch-2 脱敏负例 8/8 通过。`git commit --only -- docs/external-alpha/batch-2/` 排除了原本已暂存的 `.idea/.gitignore`，其暂存 diff 保持逐字相同。首次 commit 因没有 Git 身份配置而失败；成功重试只为该进程沿用最近提交一致的身份，没有写入 Git 配置。

本轮开始还发现 `docs/external-alpha/attribution/probe.c` 的**既有末尾换行删除**。直接对当前工作树执行 batch-2 完整校验因此在旧材料哈希检查处退出 1。没有修复该文件、改哈希或降低门禁：当前 batch-2 的独立内容/转义脱敏扫描通过；在“HEAD 原旧材料 + 待提交 batch-2”的独立快照上，未修改的完整检查器通过。提交后的干净副本采用该已提交内容。旧固定白名单面对扩展整树的拒绝仍按原设计保留，不能称其支持新增目录。

既有两个 README 修改、probe 修改、IDE 暂存与未跟踪文件全部保留。构建、包、安装、原始记录及检查脚本均位于独立私人临时工作根，本文以 `${LAB}` 表示，不保存真实绝对位置。

收尾时在已提交的干净副本再次执行 batch-2 完整检查器和 `tests/check_regression_freeze.py`，均退出 0。原工作树的 **163 个既有文件**摘要、原暂存 diff、Git 本地配置均未变；唯一新增未跟踪文件是本报告。本报告通过普通文本/hex/转义路径脱敏扫描和本地链接检查；工作树 `git diff --check` 无报错。报告未暂存、未提交。

## 2. 已实际验证

### 环境与产物

通过本地独立 clone 获取上述提交，起止 `git status --porcelain` 均为空；不带入原工作树修改。使用新的 Cargo 缓存和外置 target，执行的构建命令为 **`cargo build --release`**，无额外 RUSTFLAGS。它不是在原仓库现有 target 上增量构建。

| 项目 | 实测 |
|---|---|
| 平台 | x86_64 Linux，内核 `7.2.5-200.fc44.x86_64` |
| Rust / Cargo | `rustc 1.98.1 (48a229cea 2026-09-01)`；`cargo 1.98.1 (797e8a9bc 2026-08-05)` |
| strace | 系统 PATH 无 strace；真实采集显式使用现有 strace 6.16 |
| release 构建 | 退出 0，Cargo 报告 7.07 秒；不作性能基准结论 |
| Cargo.lock | 构建前后未改；SHA-256 `8290794269e3c0a6a91a8f2e229513372043230955969d5d2ee4314e85297eee` |
| release 二进制 | 2,048,072 字节；SHA-256 `c99f7d7dcd5ed97b975962f1be2f3af5730b48e7bec4f118ca9c3d628a94c91d` |
| 版本输出 | `RunDelta 0.1.0`，退出 0；尚非预发布版本号 |

这只是本机环境验收，不是干净操作系统/容器、多发行版或可重现二进制构建证明。

### CLI 与 0/1/2/3

共执行 **43 次 CLI 调用**，包括 7 次真实成功启动的采集、文本/JSON 读写工作流和临时兼容/损坏夹具。这里的 43 不是 Rust 单元测试数量。目标是独立临时目录中的小型 C 探针：同一绝对 `open` 命令在文件存在与删除时分别退出 0/1；另一模式执行相对 `access`，产生真实 CWD 不确定性。没有使用模拟采集器代替这些采集。

| 验证 | 实际行为 | 结论类型 |
|---|---|---|
| `--help`、`diff --help`、`--version` | 退出 0；帮助列出 record/list/show/delete/diff，diff 明列 0/1/2/3 | 已实测 |
| PATH 无 strace，且未指定 RUNDELTA_STRACE | record 退出 2，诊断 `strace unavailable; install strace or set RUNDELTA_STRACE to its executable`；目标 touch 标记未出现 | 已实测，无无追踪回退 |
| good、repeat | 相同命令与 CWD，各 6 个事件、无告警、completed / complete_for_supported_events / verified，record=0 | 已实测 |
| bad | 同一命令，删除目标文件后 record=1；仍 completed、采集完整、存储 verified | 已实测，目标失败不等于采集失败 |
| good→repeat | 文本/JSON 均 diff=0；observed_differences=false | 已实测 |
| good→bad | 文本/JSON 均 diff=1；2 条差异：退出 0→1、同一 openat 的 success→ENOENT | 已实测 |
| list / show 文本与 JSON | 健康记录退出 0，存储 verified；3 个初始条目均可枚举 | 已实测 |
| 相对 access 的 partial 记录自比 | record=0、show=0，但 diff 文本/JSON 均 3；采集 incomplete、存储 verified、未观察差异 | 已实测，退出 0 的 record/show 不是完整性证明 |
| 独立副本 events 删掉完整末行 | diff 文本/JSON 均 2、observed_differences=null、storage=corrupt；show=2 | 已实测的存储故障注入，不是原记录损坏 |
| 损坏副本 list / 按 ID delete | list=0 且明确标 corrupt；delete=0 | 已实测，仅删除临时夹具 |
| v3 副本移除 finalized | show=0；diff=3，capture=unknown / storage=unfinalized | 已实测的未完成夹具 |
| 合成 v1 旧字段缺失、事件 v1 | 能读；capture=unknown / storage=unverified / diff=3 | 已实测兼容边界，不是重新执行历史 v1 writer |
| 合成 v2、无 finalized | 能读；capture=complete / storage=unverified / diff=3 | 已实测兼容边界，不升级旧记录 |
| 合成 schema 99 | diff 文本/JSON 均 2、differences=null、storage=unavailable | 已实测，不把未来格式伪装为正常 |
| 默认目录 | XDG_DATA_HOME 优先；未设时采用 HOME 下的 `.local/share/rundelta`；两种位置的 record/delete 均 0 | 已实测，独立模拟用户目录 |
| 参数/环境隐私 | 合成公开参数保留在 metadata、raw 的十六进制字符串；显式选择的公开环境值保留在 show JSON | 已实测，未使用真实秘密 |
| 运行目录权限 | 本次记录目录均 0700 | 已实测，不等于内容脱敏 |

四类 diff 与 README 的退出码表及 CLI help 一致。工具错误返回 2 与目标恰好返回 2 是不同语义，必须结合调用类型、元数据和可靠性字段。`record` 的信号/125 语义只读审查了当前实现与历史验收，本轮没有重新做信号故障注入。

实际 partial 文本顶部：

```text
Run comparison: partial → partial
Observed differences: no
Capture completeness: left=incomplete, right=incomplete
Storage integrity: left=verified, right=verified
Exit code: 3
RELIABILITY NOT ESTABLISHED: comparison cannot establish a complete, verified result.
```

对应 JSON 字段摘录，值未改变：

```json
{
  "schema_version": 4,
  "observed_differences": false,
  "reliability": {
    "left": {"capture": "incomplete", "storage": "verified"},
    "right": {"capture": "incomplete", "storage": "verified"}
  },
  "exit_code": 3,
  "errors": []
}
```

### 安装、卸载与打包

在临时根执行 `cargo install --path <干净副本> --root <临时安装根> --locked --offline`，退出 0；安装后的 `rundelta --version` 退出 0。再执行 `cargo uninstall --root <同一安装根> RunDelta`，退出 0、二进制消失，独立运行数据的 21 个文件摘要全部保持不变。**卸载包不删除 RunDelta 记录**；包名 `RunDelta` 与命令名 `rundelta` 应分别写清。[Cargo install](https://doc.rust-lang.org/cargo/commands/cargo-install.html)、[Cargo uninstall](https://doc.rust-lang.org/cargo/commands/cargo-uninstall.html)。

`cargo package --list --locked --offline` 与 `cargo package --locked --offline` 在干净副本均退出 0，后者完成包内重建验证；没有执行 publish 或 publish dry-run。实际 `.crate`：161 个文件，374,743 字节压缩大小（Cargo 显示 1.9 MiB 未压缩、366.0 KiB 压缩），SHA-256 `e3db603b9b68e4c460e5222e2eca4113d2f70f7686dadb1640d568ce3ef5f072`。包可构建不等于满足公开发布条件。

## 3. 代码审查推断与可发布的已知限制

以下来自 `src/capture/mod.rs`、`src/parse/mod.rs`、`src/store/mod.rs`、`src/diff/mod.rs`、`src/report/mod.rs` 及既有模型/验收材料。除上节列出的路径外，不把阅读实现当作本轮实测。

| 边界 | 当前可支持的声明 | 必须保留的限制 |
|---|---|---|
| Linux 与后端 | 使用 Unix/Linux API；record 要求可用 pidfd、strace 及当前选项 | Linux 5.3+ 的 pidfd 前提不等于所有内核策略/发行版均可运行；旧 strace 最低版本矩阵未测。不要宣传跨平台或内置追踪器 |
| syscall 范围 | execve/execveat；open/openat/openat2；access/faccessat/faccessat2；chdir/fchdir；clone/clone3/fork/vfork；unshare/setns/chroot/pivot_root 的上下文处理；退出与 unfinished/resumed 证据 | 不采集所有 syscall；setns/root namespace 等不能完整建模；未知、缺失、缩略或无法排序的相关证据保留警告 |
| 路径 | 当次绝对 syscall 参数、可解码 strace 目录/fd 注释提供路径拼写；cwd_before 是最近观察上下文 | 相对路径无证据时保留不确定性；不能从最后 chdir、结束后的文件系统或 inode 猜测历史绝对路径。外部未追踪 rename 不可证明 |
| 共享 FS | 已证明的 CLONE_FS 关系按 FsContext 传播/分裂，隔离私有分支 | 非 leader exec 身份迁移、PID 复用、复杂 namespace/交错仍有边界；旧记录不重新解析。见 [模型](FS_CONTEXT_MODEL.md) |
| 完整性 | capture 与 storage 分开，完整只针对支持集合；diff=3 仍可浏览观察结果 | completed、record=0、show=0、list=0 都不是全部证据完整证明；未知状态不能包装成可靠无差异 |
| 存储与兼容 | metadata v3、events v2、finalized v1、diff JSON v4；读取 metadata v1/v2/v3、events v1/v2 | v1/v2 无校验标记为 unverified；旧 CWD/共享关系语义不追溯升级。摘要不覆盖 raw，不防能重写摘要的篡改，也不保证断电持久性 |
| 原始记录敏感性 | argv、路径、显式选择的环境值会持久化；raw 十六进制可逆，文本/JSON 不自动脱敏 | 不应自动上传整个记录根；未选择环境不表示所有环境信息绝不通过参数/路径间接泄露；0700 不是加密 |
| 噪声与排名 | 运行级聚合提供线索；完整 JSON 保留排序前的全部观察集合对应条目 | 第二轮 CMake 正常重复 2,262 条、partial/diff=3；历史 Cargo 的 1,598 条及旧 incomplete 是冻结观察，不能当成当前所有 Cargo 均不完整。新 FsContext 审计并未追溯升级历史记录 |
| 内容和根因 | 比较观察路径、操作、结果、执行与退出变化 | 不诊断文件内容差异；同路径不证明同内容；打开不证明读取/影响行为；不提供通用根因判断、重放或自动修复 |

这些局限可以随限定范围 alpha 发布，前提是入口文档准确展示，不把不完整记录、CMake/Cargo 噪声或前五样例结果隐藏。第二轮 2 个案例的命中是小样本事实，不是一般命中率保证；历史结论保持不变。

生命周期仍是 RunDelta→strace→目标/子进程；PDEATHSIG、kill-on-exit 与 pidfd 的现有边界见 [架构审查](ARCHITECTURE_REVIEW.md)。本轮没有做终端、SIGKILL、守护化、特殊权限或跨命名空间重验，不扩大已有保证。

## 4. 必须在公开 alpha 前处理

### P1-A：缺少项目许可与必要发布元数据

**证据：已实测清单 + 只读 manifest。** `Cargo.toml:1` 的 package 只有 name/version/edition；根目录没有 LICENSE、COPYING 或 NOTICE。`cargo metadata` 中 description/license/license_file/repository/homepage/rust_version 均为空；readme 自动选择 README.md。Cargo 打包警告缺少 description/license 等，但本地打包仍成功。

不能仅凭依赖都是常见开源许可，就替 RunDelta 指定许可证或宣称本项目授权已明确。最小处理是由权利人选择/确认项目许可证，提供一致的文本和 manifest 字段；补充准确 description。若准备 crates.io 发布，description 与 license/license-file 是注册表要求；repository 等应填写真实已确定地址，不能放占位 URL。[Cargo manifest 官方说明](https://doc.rust-lang.org/cargo/reference/manifest.html#the-license-and-license-file-fields)。

`RunDelta 0.1.0` 不是 alpha 后缀；这是发布命名决策，不能自动证明稳定性。本轮不改版本，也未验证 crates.io 名称可用性。未设 `publish=false` 只是事实，不建议在本次审查中擅自修改发布配置。

**第三方边界：** Linux metadata 解析结果含 7 个直接依赖、38 个依赖包，均有许可证表达式；直接依赖为 MIT/Apache-2.0 双许可（signal-hook 用旧斜杠形式）。解析图包括 Unicode-3.0 的附加要求，不能只看直接依赖。这里只检查声明，没有完成逐行来源或二进制通知文件审计。

strace 是外部进程，实际 crate 清单没有其源码/二进制；本地 strace 6.16 COPYING 与官方版本均声明主程序 LGPL-2.1-or-later、测试套件 GPL-2.0-or-later，另有 Linux UAPI 头文件边界。若未来另行捆绑它或工具链，必须分别处理相关义务，不能沿用 RunDelta 自身许可覆盖。[strace 6.16 COPYING](https://raw.githubusercontent.com/strace/strace/v6.16/COPYING)。当前源码/包清单未发现 ReproZip/tracexec 项目副本；这不是完整代码来源证明。

### P1-B：发布清单与产物隐私隔离不足

**证据：实际打包与字节扫描。** 未配置 include/exclude，干净提交的包包含 120 个 docs 文件、22 个 tests 文件。四份历史材料里共 **28 行**含当前原项目的真实绝对路径，已经在实际 `.crate` 内再次确认，而不是只凭正则命中通用示例：

| 文件 | 首个确认位置 | 风险 |
|---|---:|---|
| `docs/ACCEPTANCE-before-reliability.md` | 188 | 本机审计产物位置 |
| `docs/FIELD_CASES_RESULTS.json` | 2 | 本机工作根 |
| `docs/HOLDOUT_RESULTS.json` | 5 | 本机运行与依赖路径，多处 |
| `docs/RANKING_REPAIR_RESULTS.json` | 2 | 本机工作根及观察路径，多处 |

本文不复述这些私人路径。包里还存在临时目录引用，例如 `docs/performance/a3-completion-check/compare.py:4`；另一些临时路径来自合成测试，不能全都认定为真实隐私泄漏，但都应纳入发布清单审阅。

| 产物/操作 | 实际结果 |
|---|---|
| 干净 HEAD 的 crate | 161 文件；无 IDE、target、venv、完整 `raw/trace.log`；有上述私人路径材料 |
| 原工作树状态的独立快照，`cargo package --list --allow-dirty` | 165 文件；额外纳入 `.idea/.gitignore`、RunDelta.iml、modules.xml、vcs.xml |
| 同一脏快照，默认 `cargo package` | 退出 101，明确拒绝 7 个未提交文件；这是 Cargo 防护，不是 RunDelta 退出码 |
| 测试资产 | 22 文件会进入包；包括 `tests/fixtures/trace.txt` 合成追踪 fixture，不是本次私人 raw |
| 默认 release 二进制 | strings 中有本次临时构建/缓存根 32 条、用户根路径 20 条；原项目绝对根未嵌入，但仍不能原样当成已脱敏产物 |

有限的明文/hex 与凭据形状扫描未发现匹配的真实 token/私钥；不构成“绝无秘密”的证明。尤其**batch-2 自身脱敏通过，并不证明整个源码包已脱敏**。Cargo 默认文件选择及核查命令见 [include/exclude](https://doc.rust-lang.org/cargo/reference/manifest.html#the-exclude-and-include-fields)、[cargo package](https://doc.rust-lang.org/cargo/commands/cargo-package.html)。

最小处理方向是定义明确的发布文件清单、检查实际归档及二进制。保留冻结记录原件和哈希，不在本轮重写或删除；另行授权建立适合分发的发布树/脱敏副本。若公开的是整个 Git 仓库或历史，仅从 crate 排除文件并不能解决问题。预编译 Rust 库也可能嵌入路径，不能假设只设置源码路径映射或 strip 就已经解决。所有调整都留待后续授权，本轮未实施。

### P1-C：发布入口文档与当前实现不一致

**证据：同时审阅提交版与保留的工作版 README。** 提交版 `README.md:126` 仍描述从初始 cwd/chdir 推导相对绝对路径；工作版虽已有不确定性说明，但两版仍称一次 CLONE_FS 会禁用整次运行 CWD 回退。两版对 unshare 的描述也没有反映已实现的私有 FsContext 分裂。这与当前代码及 [FsContext 模型](FS_CONTEXT_MODEL.md) 不一致。

历史 Cargo 的 incomplete/diff=3 结论应保留，但必须明确它是旧记录；不能把“Cargo 仍因共享 FS 不完整”当成当前所有新采集的产品性质。阶段二/旧测试数量是历史快照，也应与当前准备发布的构建区分。存储章节“写入失败不应出现完成标记”的绝对措辞还需对照架构审查中 marker rename 后目录同步失败的边界，避免扩大断电或 I/O 保证。

安装说明只有开发构建路径，没有完整的本地安装、卸载与数据保留说明。示例中的 `target/tools/strace-6.16/src/strace` 不随源码包提供，不能作为外部用户开箱可用的唯一路径。最小文档修正应使用已安装的受支持 strace、说明 `RUNDELTA_STRACE` 的可选覆盖，区分包名/二进制名，补上卸载不删数据和 diff=3 的读取方式。无需因此扩充产品功能。本轮遵照约束没有修改 README。

## 5. 尚未验证与可按范围限制发布

- **P2 / 平台范围：** 只测本机 x86_64 Linux、Rust 1.98.1、strace 6.16。未测最低 Rust/strace、其他架构、musl、旧 glibc、容器安全策略。实际 ELF 动态依赖 libc、libgcc_s 与 loader，导入版本包含 GLIBC_2.39；不能把本机二进制标为任意 Linux 通用包。源码 alpha 可以声明精确已测环境，不先承诺多平台二进制。
- **P2 / 分发与供应链：** 未执行 crates.io publish/dry-run、名称查重、GitHub Release、签名、SBOM 或可重复二进制验证；未完成全部传递依赖/工具链通知整理及源代码逐行来源审计。没有依据推断注册表上传一定通过。
- **P2 / 复杂行为：** 本轮没有重跑 Rust 单元、六个平台入口、生命周期专项、性能或外部项目；既有审计结论仅引用，不冒充本轮新结果。兼容夹具覆盖常规版本读取，不覆盖所有历史记录组合或所有存储故障。
- **已知限制：** 不完整采集、历史存储未经验证、动态构建噪声、未追踪目录重命名、文件内容与根因不可证明，都可在限定范围 alpha 中透明保留。不能为发布把警告豁免为完整，也不能把前五样例命中当作保证。

未发现必须先改产品逻辑才能进行限定范围试用的新缺陷。先完成 P1-A/B/C，再复验最终发布树/包/二进制与安装文档，比继续新增功能更有价值。

## 6. 可复现命令

以下使用由操作者设置的 `SOURCE`（上述提交的干净副本）、`LAB`（独立空工作根）、`STRACE`（受支持的现有可执行文件）。所有路径变量必须指向临时审查环境，不在原工作树创建 target 或安装到用户默认目录。

```bash
cd "$SOURCE"
env CARGO_HOME="$LAB/cargo-home" CARGO_TARGET_DIR="$LAB/build" cargo build --release
BIN="$LAB/build/release/rundelta"
"$BIN" --help
"$BIN" diff --help
"$BIN" --version

env CARGO_HOME="$LAB/cargo-home" CARGO_TARGET_DIR="$LAB/build" \
  cargo package --list --locked --offline
env CARGO_HOME="$LAB/cargo-home" CARGO_TARGET_DIR="$LAB/build" \
  cargo package --locked --offline
env CARGO_HOME="$LAB/cargo-home" CARGO_TARGET_DIR="$LAB/build" \
  cargo install --path "$SOURCE" --root "$LAB/install" --locked --offline
"$LAB/install/bin/rundelta" --version
env CARGO_HOME="$LAB/cargo-home" cargo uninstall --root "$LAB/install" RunDelta
```

最小工作流使用下列探针保留同一入口命令；它是本报告的临时测试代码，不进入产品或现有回归集：

```c
#include <fcntl.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc < 3) return 64;
    if (strcmp(argv[1], "access") == 0)
        return access(argv[2], F_OK) == 0 ? 0 : 1;
    int fd = open(argv[2], O_RDONLY);
    if (fd < 0) return 1;
    close(fd);
    return 0;
}
```

将探针保存为 `$LAB/smoke.c`。以下脚本含预期非零命令，不应使用遇非零立即停止的 `set -e`：

```bash
cc -O0 -o "$LAB/probe" "$LAB/smoke.c"
cd "$LAB"
printf 'public demonstration\n' > payload.txt
export RUNDELTA_STRACE="$STRACE"
"$BIN" --storage "$LAB/records" record good -- "$LAB/probe" open "$LAB/payload.txt"
"$BIN" --storage "$LAB/records" record repeat -- "$LAB/probe" open "$LAB/payload.txt"
rm -- "$LAB/payload.txt"
"$BIN" --storage "$LAB/records" record bad -- "$LAB/probe" open "$LAB/payload.txt" # 1
"$BIN" --storage "$LAB/records" diff good repeat                              # 0
"$BIN" --storage "$LAB/records" diff good bad                                 # 1
"$BIN" --storage "$LAB/records" diff good bad --json                          # 1
"$BIN" --storage "$LAB/records" list
"$BIN" --storage "$LAB/records" list --json
"$BIN" --storage "$LAB/records" show good
"$BIN" --storage "$LAB/records" show good --json
printf 'public demonstration\n' > payload.txt
"$BIN" --storage "$LAB/records" record partial -- "$LAB/probe" access payload.txt
"$BIN" --storage "$LAB/records" diff partial partial                          # 3
"$BIN" --storage "$LAB/records" diff partial partial --json                   # 3

mkdir "$LAB/empty-bin"
env -u RUNDELTA_STRACE PATH="$LAB/empty-bin" "$BIN" --storage "$LAB/missing" \
  record missing -- /usr/bin/touch "$LAB/should-not-exist"                      # 2
test ! -e "$LAB/should-not-exist"
```

存储与旧格式复现只操作上述新建 good 目录的**独立副本**：events 去掉完整末行触发 corrupt/2；v3 移除 finalized 触发 unfinalized/3；v1/v2 夹具移除标记、设相应 metadata 版本，v1 还移除 environment/completeness 及事件可选字段并使用 event v1，分别读为 unverified/3。源码和旧记录从不原地改写。完整私人命令输出留在临时工作根；公开报告仅保留脱敏结果。

## 7. 推荐的发布声明范围

完成三项 P1 门禁后，可采用如下范围声明，不能用它替代实际发布复验：

> RunDelta 是面向 Linux 的探索性命令运行差异工具，依赖外部 strace。它对两次运行中支持集合内的执行路径、文件访问结果和退出信息提供可回溯观察线索。采集不完整或存储未经验证时会明确标注，diff 返回 3；损坏记录返回 2。它不比较任意文件内容，不证明故障根因，不自动修复，也不保证真实线索总在前五。原始记录可能包含敏感参数、路径和选定环境值，分享前需审查。

因此，**继续准备：可以；按当前包/二进制直接公开发布：不建议，P1-A/B/C 尚未完成。** 第二轮诊断证据的正面结果与发布材料尚未就绪是两个不同结论，本报告不改写任何既有外部案例结论。
