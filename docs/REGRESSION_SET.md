> 公开树说明（2026-10-16）：本树引用的 ACCEPTANCE-before-reliability、FIELD_CASES、HOLDOUT_RESULTS、RANKING_REPAIR 及对应结果 JSON 为脱敏派生副本；原始证据保留在私有归档，未重新实测。下文关于私有树、历史路径或发布阻塞的结论属于当时状态，不代表本公开副本仍含这些路径。公开派生合同见 [REGRESSION_SET_PUBLIC.json](REGRESSION_SET_PUBLIC.json)；原 v1 原件校验仍属私有树。
> Public references point to redacted historical derivatives, not fresh measurements. Original v1 evidence verification remains private; the public archive has a separate integrity contract.

# 固定回归集 v1

以下五例已参与规则设计，从现在起只作为回归门禁，不再作为调排序样本或泛化效果证据：

1. python-venv：隔离虚拟环境选错解释器。
2. pkg-config：已安装依赖的搜索目录变化。
3. git-identity：用户身份配置丢失。
4. gcc-header：真实 strace.c 的必需头文件丢失。
5. cargo-linker：RunDelta 离线构建指定不存在的链接器。

冻结生成器为 examples/field_cases.py，冻结历史检测项为 tests/field_report_audit.py；冻结摘要及 SHA-256 在 REGRESSION_SET.json。不得以这五例为依据调整排序、扩大临时路径规则或减少检测项。基础设施迁移若必须更新脚本，应单独解释并保留行为断言；不能覆盖旧成绩。历史冻结与当前新鲜采集是两种不同证明，详见 [五案例审计分层](FIELD_AUDIT_LAYERS.md)。

从仓库根目录复现（Linux、本机工具/缓存前置条件见 FIELD_CASES.md）：

```bash
cargo build --offline
# FROZEN 是既有、未经改写的历史五案例目录。
python3 tests/field_report_audit.py "$FROZEN"

# 生成器打印本次新目录；不要把它交给旧审计。
RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace" python3 examples/field_cases.py
python3 tests/fresh_field_audit.py "$FRESH"
```

两个入口都检查各例直接线索、正常重复、存储与文本/JSON 一致性，但不能相互替代。冻结 Cargo 记录中的旧全局共享-FS warning 与 diff 3 必须原样保留；当前新鲜 Cargo 契约要求零 warning、capture complete、storage verified，正常→异常 diff 为 1。不得改写冻结事件/坐标来迁就新 parser，也不得删除当前产生的真实不确定性来强行达到 0/1。数量受工具版本、调度和路径变化影响，不将历史精确数量作为通用门禁。
