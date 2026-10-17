# 第二轮外部 alpha：同一入口、单一条件

检查与执行日期：2026-09-20。本轮选中 **2 个当日仍 open 的公开 issue**，没有为了凑满 3 个而构造混杂对照。只读 issue 正文、状态、标签、许可证、贡献指南和复现必要源码；没有读取评论、PR 或提交讨论，也没有对外操作。筛选细节见 [SCREENING.md](SCREENING.md)。

| 案例 | 原 issue 是否复现 | 对照是否有效 | capture 状态 | 前五命中 | 可定位程度 | 主要限制 |
|---|---|---|---|---|---|---|
| [pip #12996](https://github.com/pypa/pip/issues/12996) | 是：正文配置查询症状 2 | 是：同一 pip argv/CWD，仅 PIP_CONFIG_FILE 改变 | 完整，storage verified；异常比较 diff=1 | 是：覆盖配置打开第 2；用户配置不再被观察第 3 | 配置选择层的直接文件访问线索 | 未执行包安装；不能证明内部配置优先级或设计缺陷 |
| [bracket-lib #376](https://github.com/amethyst/bracket-lib/issues/376) | 部分：原 expat-sys 依赖的配置失败阶段；未复现完整应用 | 是：同一 cmake argv/CWD/源码，仅 PATH 的 CMake 提供者改变 | partial，storage verified；三种比较均 diff=3 | 是：新 CMake 成功执行第 3 | 实际工具链提供者变化 | 相对 access 的 CWD 不确定性；正常重复有 2,262 条差异；不是完整 Cargo 构建 |

这是实际采集结果和操作者定位判断，**没有独立用户盲测**。前五位置来自完整 JSON 的原始 `changes` 顺序，没有筛选、折叠、重排，也没有预先选择环境变量来制造命中。

## 控制与证据边界

每例先写 [pip CASE](pip-12996/CASE.md) / [bracket-lib CASE](bracket-lib-376/CASE.md)，登记精确入口、唯一变量、症状和关键线索，再开始采集。其摘要与首次私有采集登记一致，未事后修改。每例运行 good、repeat、bad、bad-repeat，比较正常→正常、正常→异常、异常→异常。

所有运行使用同一个现有 RunDelta 二进制；源码、排序、解析、存储及审计均未修改。RunDelta commit、二进制、strace、下载包和事前登记摘要见 [provenance.json](provenance.json)。目标项目/依赖源码版本固定。CMake 的工具版本变化就是唯一 PATH 条件，不是目标项目版本变化；没有在两侧切换 Meson/Cargo 等入口。

第三方发布包、虚拟环境、构建目录、完整 JSON/text、stdout/stderr 和 raw 均只在独立临时工作目录中。所有现存记录保留，分析前后逐文件核对摘要。仓库中只有脱敏摘要和本次局部证据坐标；没有第三方源码、完整 raw 或构建产物。`${WORK}` 是独立工作根，`${CASE}` 是其案例子目录，不是历史真实路径。摘要中的 `raw/trace.log` 只是证据位置，不是随附日志。

当次标准化事件与原始调用核对包括 PID 角色、操作、绝对操作数和成功返回；unfinished/resumed 仅拼接本记录内同一 PID、同一 syscall 的对应端点。重采集不要求与历史行号相等，也不把本次证据坐标当作公开 raw 已提供。

## 实际结果与重跑

| 比较 | pip 差异数 / diff 退出码 | CMake 差异数 / diff 退出码 |
|---|---:|---:|
| 正常→正常 | 0 / 0 | 2,262 / 3 |
| 正常→异常 | 3 / 1 | 2,072 / 3 |
| 异常→异常 | 0 / 0 | 242 / 3 |

四次 pip 均无告警。四次 CMake 各有一条相对 `access("cmake", X_OK)` 的路径不确定性告警，不能拼接成已知物理绝对路径；没有额外未知 syscall、采集器异常或存储失败。RunDelta 文本明确显示 `RELIABILITY NOT ESTABLISHED`，JSON 的 capture=incomplete 与退出码 3 一致。命中绝对 execve 证据不能升级整次采集状态。

CMake 前五中还出现一次查找 `as` 的 ENOENT，但同次失败运行另有 4 次 `/usr/bin/as` 成功执行；它不是汇编器缺失的证据。正常重复的大量工作路径差异仍完整计入噪声，未当作可忽略规则。详见 [pip 结果](pip-12996/RESULTS.md)、[CMake 结果](bracket-lib-376/RESULTS.md) 及各自 summary/evidence JSON。

首次 8 条记录完成后，使用公开脚本默认入口，在**另一个全新工作目录**重新下载、安装并采集 8 条。两批的目标退出、capture/storage、事件数、三组差异数和关键排名全部相同。第二批没有覆盖第一批公开摘要；这只证明本机两次独立准备可复现，不代表跨平台或未来依赖状态均相同。

## 公开复现

条件：Linux x86_64、系统 Python ≥3.12（含 venv/pip）、cc/c++/make，以及已构建的 RunDelta 和支持本产品选项的 strace。安装阶段需要访问公开 PyPI、Python 发布包和 crates CDN；被记录命令不需要网络、账户或高权限。脚本在系统临时目录自动新建工作根，不修改本仓库或第三方源文件，不覆盖既有记录。

在仓库根目录，指定现有二进制：

```bash
python3 -B docs/external-alpha/batch-2/reproduce.py \
  --rundelta "$RUNDELTA" --strace "$STRACE"
```

本次实际使用的相对路径为 `target/debug/rundelta` 与 `target/tools/strace-6.16/src/strace`，文件摘要在 provenance 中；脚本不自动构建或替换它们。准备阶段使用 uv 0.12.17 获取 CPython 3.12.11，固定 pip/CMake wheel 和 expat-sys crate 摘要。uv/Python 安装器版本固定但不构成全工具链可重现构建；系统编译器和内核条件也应记录。

脚本打印新建的私人工作根。将其在本机赋给 `WORK`，生成另一个尚不存在的分析目录：

```bash
python3 -B docs/external-alpha/batch-2/analyze.py "$WORK" \
  --output "$WORK/public-summary"
```

不要将整个工作根或未经核查的分析输出复制进仓库。完整目标 argv 在各 CASE 中；[reproduce.py](reproduce.py) 保留全部参数边界，并检查环境差别恰为一个键、命令/CWD 一致以及源码/工具/fixture 未改变。症状和排名由独立 [analyze.py](analyze.py) 核对，采集脚本通过本身不等于原 issue 已复现。

材料校验和适用范围见 [CHECKS.md](CHECKS.md)。现有固定白名单不被扩充；新增目录使用独立 [verify.py](verify.py)，普通文本和 hex/转义内容均检查。

## 判断与下一步

- **第二轮提高了公开 alpha 的证据质量。** 两例都有同入口、单变量、正常重复和异常重复，关键事实均在前五；结果不再依赖不同构建系统间的混杂比较。样本只有两例，不能把 2/2 当成一般命中率。
- **适合的类别**：配置选择路径变化、实际可执行程序/工具链路径漂移，且能建立本地相同入口对照。文件打开、执行路径和退出状态有直接证据，但内容语义仍需另行验证。
- **当前不适合的类别**：无法建立同入口正常运行、需要额外未具备的平台/权限条件、主要依赖内容或内部逻辑而没有路径变化的问题。大量动态构建路径和不完整采集可供浏览，但不适合作为“完整且低噪声诊断”的展示证据。
- **公开 alpha 建议**：具备支持有限范围、明确标注可靠性状态的探索性公开 alpha 的诊断证据；还不足以宣称通用构建故障定位或稳定前五保证。CMake 的 partial/退出 3 和正常重复噪声应随案例公开。本轮没有确认必须先修复的新产品缺陷；也没有推翻旧 alpha 报告的结论。
- **贡献准备建议**：pip 可进入独立配置语义与文档一致性验证；bracket-lib 可进入完整应用的依赖/工具链根因验证。两者都未到直接准备修复 PR 的证据门槛。本轮未读取上游讨论，也未判定上游必然存在实现缺陷。

这里只新增本目录材料；不暂存、提交或推送，不修改旧文档结论。
