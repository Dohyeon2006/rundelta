# 本轮验证结果

2026-09-20，以下均为实际执行；没有使用模拟数据或修改断言使案例通过。

| 检查 | 结果 |
|---|---|
| 首轮三例及 SPAdes 重试 | 共 10 条真实运行记录；3 个主比较和 2 个保留的尝试比较 |
| 独立复现脚本 | 在第二个全新隔离目录完整执行，脚本退出 0；又生成 10 条记录；主比较目标 rc 仍为 0/1、0/1、0/4，diff 仍为 1/3/3 |
| 严格前五评估复算 | pybind11 命中第 2；Meson 不命中；SPAdes 未命中原登记的具体异常证据、CWD 第 2 只部分有用 |
| 重试保留 | Python 启动失败 127/127、成功对照 0/0 均保留；没有把它们说成正常→异常 |
| 重放波动 | pybind11 635、Meson 856、SPAdes 最终 3659 条不变；SPAdes 成功→成功由 6287 变 6288；参见 REPLAY.json |
| 新材料校验 | `python3 -B docs/external-alpha/verify.py` 通过；含公开文件白名单、摘要数量、可靠性、原登记哈希、解码后私有路径/凭据模式扫描 |
| 原五案例冻结材料 | `python3 -B tests/check_regression_freeze.py` 通过，v1 材料与 v2 审计契约保留 |
| 外部 #5626 冻结材料 | `python3 -B docs/external-cases/pybind11-5626/verify.py` 通过，446 keys / 712 calls / 420 paths |
| A3 基线 | `python3 -B docs/performance/a3-completion-check/verify.py` 通过 |
| A3 优化材料 | `python3 -B docs/performance/a3-completion-check/verify_optimization.py` 通过 |
| 原工作区保留 | 100 个受保护文件（已跟踪文件、指定 IDE 文件、现有二进制、仓库配置）SHA-256 不变；暂存区 binary diff 字节不变 |
| 第三方源码 | 三个 checkout 均 clean；首轮 Meson fixture 文件哈希不变；两份 SPAdes 安装的原有文件在所有尝试后与官方压缩包逐文件一致 |

脱敏扫描是具体模式检查和人工复核，不声称能够检测任何形式的秘密。这里只记录验证结果及少量脱敏观察，不附完整输出、虚拟环境或原始追踪。

未执行：独立人类用户盲测、发行版矩阵、未追踪运行对照、第三方完整测试套件、内存/时序无扰动证明、所有未关联修复 PR 的排查。未进入根因源码审查或补丁验证，也没有重建或修改 RunDelta 产品。旧产品测试未重跑，因为本阶段只读且没有产品改动。

`CASE.md` 的 open 状态只代表检查时刻；未来重放应重新只读检查正文/状态/标签。此脚本复现固定版本行为，不自动读取新的评论或保证 issue 在未来仍然 open。

## 独立重放中的额外不确定性

SPAdes 的第二阶段成功运行 `bad-v2` 在重放中新增 `unfinished call at end of trace` 告警（原始第 24101 行）。实际追踪是 `???( <unfinished ...>`，同一 PID 第 24107 行正常退出；本轮没有识别这个 syscall，也未证明它是 RunDelta 解析器缺陷。目标仍退出 0，存储仍 verified，采集仍 incomplete，diff 仍为 3。全部告警及两行脱敏证据保留在 [REPLAY.json](REPLAY.json)。新增的一条 Capture completeness 差异解释了总数 6287→6288，不能仅归为无害临时文件噪声。后续应独立复现并分类这条未知调用，不能删除告警使报告“完整”。
