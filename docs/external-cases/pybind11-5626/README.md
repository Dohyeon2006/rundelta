# pybind11 #5626：冻结的外部诊断案例

**原 issue 未复现。** 本目录仅公开用于 RunDelta 诊断评估的脱敏证据和分析，不能作为 pybind11 缺陷证据。

- [冻结说明](FROZEN.md)：范围、不可改变的历史结果及使用边界。
- [验证报告](REPORT.md)：已知信息、独立观察、推断、无法证明的内容。
- [446 条差异分析](NOISE_ANALYSIS.md)：十类来源、次数/父进程/结果统计，以及未来折叠规则草案和风险。
- [最小证据](evidence.json)：版本、原结果、前五条摘录、第 177 条头文件证据。
- [446 行脱敏台账](repeat-differences.csv)、[父进程统计](parent-processes.csv)、[汇总](noise-summary.json)。

离线复核不需要第三方源码或原始追踪：

```bash
python3 docs/external-cases/pybind11-5626/verify.py
python3 docs/external-cases/pybind11-5626/summarize.py
```

公开复现使用 [reproduce.py](reproduce.py)，需要可用的 uv、RunDelta、strace、make 和 C++ 编译器、网络，以及支持对应 Python 二进制的 Linux x86_64。原测 uv 为 0.12.17。示例：

```bash
UV_BIN="$(command -v uv)" \
RUNDELTA_BIN="$PWD/target/debug/rundelta" \
RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace" \
python3 docs/external-cases/pybind11-5626/reproduce.py
```

脚本只读取 issue 正文 API，不读取评论；校验两个最小代码片段的 SHA-256 后，才在新建临时目录生成源文件。下载的 Python/pybind11/CMake、缓存、构建物、记录和日志全部留在临时目录，不写入本仓库。临时结果可能含本机路径，不应直接发布。脚本基于已执行流程整理，本轮仅做语法/结构检查，未重新下载并运行整套构建。

版本、工具链和随机路径会影响重跑数量/名次；**177 和 446 是冻结的历史实测值，不是跨环境保证，也不是为了让复现通过而应调整的排序目标。**

## 公开材料边界

`${CASE}` 替代原实验根目录，`${TMP}` 替代临时目录；没有真实用户名、私有绝对路径、数字 PID、完整 argv、环境快照、文件内容或完整追踪。父进程用单次运行内的 L/R 匿名编号，不能跨运行按编号匹配。仅保留可验证关键结论和复算分析所需的数据。

台账包含 446 个已变化的聚合键，不是全部事件；actor/parent/outcome 次数由原事件与原始行的访问标志提取。原始证据只在私有实验中核对过，公开台账不能取代它来审计解析器。原私有 diff 的 SHA-256 仅作来源标识，不是签名或防篡改保证。
