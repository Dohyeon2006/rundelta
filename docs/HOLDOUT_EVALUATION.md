> 本地证据路径说明：本文及本索引所链接历史文档中的 `target/`、本机实验目录和未发布原始日志路径，仅用于说明原本地复现实验位置；这些产物不随公开版提供，不是可访问的公开证据链接。公开可核验材料以仓库实际收录的文件和公开派生合同为准，不能据本地路径推断已发布原始记录。
> Local evidence paths: `target/`, local experiment directories and unpublished logs describe original local reproductions. Their artifacts are not shipped with this public edition and are not accessible public evidence links.

> 公开树说明（2026-10-16）：本树引用的 ACCEPTANCE-before-reliability、FIELD_CASES、HOLDOUT_RESULTS、RANKING_REPAIR 及对应结果 JSON 为脱敏派生副本；原始证据保留在私有归档，未重新实测。下文关于私有树、历史路径或发布阻塞的结论属于当时状态，不代表本公开副本仍含这些路径。公开派生合同见 [REGRESSION_SET_PUBLIC.json](REGRESSION_SET_PUBLIC.json)；原 v1 原件校验仍属私有树。
> Public references point to redacted historical derivatives, not fresh measurements. Original v1 evidence verification remains private; the public archive has a separate integrity contract.

# 第二批独立案例评估

已按 [执行前登记](HOLDOUT_PLAN.md) 完成五个此前未用于规则设计的真实工具流程，先 good、再正常 repeat、最后 bad，共 15 份新记录。整个评估没有修改 src、依赖或排序规则；计划与代码摘要保存在 [结果/manifest](HOLDOUT_RESULTS.json)，执行结束已再次校验一致。原五例已固定为 [回归集 v1](REGRESSION_SET.md)，不再用于调排序。

本批是本机实际工具的受控故障，不是外部用户事故合集。关键线索前五条命中 **4/5**；“能否定位”是分析者依据报告的判断，**没有独立用户实验**，不能把命中率等同于用户成功率。保留失败案例，不为提高分数更换任务或调整规则。

| 新案例 | 目标退出码 | 前五命中 | 首条关键线索排名 | 能否据此定位（分析者判断） |
|---|---|---|---|---|
| OpenSSL CA 哈希信任链接丢失 | 0→2 | 是 | 3 | 部分：知道检查哪一个 CA 路径，未得到缺失结果 |
| Python 当前目录模块遮蔽标准库 | 0→1 | 是 | 2 | 可定位：检查新出现的本地 json.py |
| tar 输出目录失去写权限 | 0→2 | 是 | 2 | 可定位：目标文件 success→EACCES |
| Git shared clone 的供体对象库丢失 | 0→128 | 是 | 2 | 部分：知道供体对象库未再访问，未证明目录被移走 |
| sha256sum 的同路径归档内容损坏 | 0→1 | 否 | 无 | 不能：报告只有退出码变化 |

五例采集均 complete_for_supported_events、存储 verified，故障比较 diff 全部返回 1；正常重复全部 0 条变化、退出码 0。这里的 complete 限于已支持事件，不意味着 stat、内容或完整运行条件已被采集。新案例数量有限，不宣称一般化成功率。

## 各例实际前五条与局限

以下原样取 JSON changes 的顺序；共同路径前缀以 `$WORK` 缩写。退出状态占名次但不算关键线索，目标 stderr 只用于验证注入确实生效。

### openssl-trust

| 排名 | 类别 | 对象 | 观察结果 |
|---:|---|---|---|
| 1 | Exit status | `—` | code=Some(0), signal=None → code=Some(2), signal=None |
| 2 | Removed file access | `/etc/localtime (openat)` | success → 未观察到 |
| 3 | Removed file access | `$WORK/openssl-trust/trust/a7943d2e.0 (openat)` | success → 未观察到 |

定位判断：检查 trust/a7943d2e.0 是否存在且指向正确 CA；报告只显示不再打开，不能确定是链接删除、不同链还是其他证书变化。

噪声、遗漏与边界：/etc/localtime 消失排在关键路径前；未记录失败 stat，缺少 ENOENT。

### python-shadow

| 排名 | 类别 | 对象 | 观察结果 |
|---:|---|---|---|
| 1 | Exit status | `—` | code=Some(0), signal=None → code=Some(1), signal=None |
| 2 | New file access | `$WORK/python-shadow/json.py (openat)` | 未观察到 → success |
| 3 | New file access | `/lib64/libgcc_s.so.1 (openat)` | 未观察到 → success |
| 4 | New file access | `/lib64/libsystemd.so.0 (openat)` | 未观察到 → success |
| 5 | New file access | `/lib64/libuuid.so.1 (openat)` | 未观察到 → success |

定位判断：检查当前目录 json.py 与标准库模块重名；新本地模块成功打开直接给出可检查对象。

噪声、遗漏与边界：异常处理新增 libgcc_s/libsystemd/libuuid 等库挤占后续名次；不能从路径证明模块内容。

### tar-permission

| 排名 | 类别 | 对象 | 观察结果 |
|---:|---|---|---|
| 1 | Exit status | `—` | code=Some(0), signal=None → code=Some(2), signal=None |
| 2 | File access outcome changes | `$WORK/tar-permission/output/README.md (openat)` | success → EACCES |

定位判断：检查 output/README.md 及父目录写入权限；success→EACCES 可定位具体写入失败对象。

噪声、遗漏与边界：不能单凭 EACCES 区分 mode、ACL 与 SELinux；实际 chmod 注入只用于验证真值。

### git-alternates

| 排名 | 类别 | 对象 | 观察结果 |
|---:|---|---|---|
| 1 | Exit status | `—` | code=Some(0), signal=None → code=Some(128), signal=None |
| 2 | Removed file access | `$WORK/git-alternates/donor/.git/objects/03/8b6633b3088579cd07b6ec0eba2585008ebdf3 (openat)` | success → 未观察到 |
| 3 | Removed file access | `$WORK/git-alternates/donor/.git/objects/35/dbbbd74e944e6f5af9662b4d38408644cb7215 (openat)` | success → 未观察到 |
| 4 | Removed file access | `$WORK/git-alternates/donor/.git/objects/d8/cbe2bb7d3955142842e9c5ca74aaf8d5a4abf6 (openat)` | success → 未观察到 |
| 5 | Removed file access | `$WORK/git-alternates/donor/.git/objects/pack (openat)` | success → 未观察到 |

定位判断：检查 donor/.git/objects 及克隆的 objects/info/alternates；报告指向供体对象库，但没有明确展示目录不存在。

噪声、遗漏与边界：多个对象消失重复占位；失败 stat 不在支持范围，缺少具体缺失结果，不能从成功访问消失推断唯一原因。

### checksum-content

| 排名 | 类别 | 对象 | 观察结果 |
|---:|---|---|---|
| 1 | Exit status | `—` | code=Some(0), signal=None → code=Some(1), signal=None |

定位判断：报告只有退出码变化，不能单独定位 source.tar 字节漂移；需另查校验工具输出。

噪声、遗漏与边界：路径和 open 成功结果不变；内容漂移是明确能力边界，不能把存储摘要等同于被测文件内容快照。

## 实际诊断输出

目标程序输出摘录，不冒充 RunDelta 的根因解释：

```text
OpenSSL: error 20 at 0 depth lookup: unable to get local issuer certificate
Python: AttributeError: module 'json' has no attribute 'dumps'
tar: README.md: Cannot open: Permission denied
Git: error: object directory .../donor/.git/objects does not exist; check .git/objects/info/alternates
sha256sum: source.tar: FAILED
sha256sum: WARNING: 1 computed checksum did NOT match
```

OpenSSL 与 Git 只有成功访问消失，没有记录失败 stat；不能由“未再打开”断言唯一原因。Python 后面的异常处理库访问是噪声。tar 的 EACCES 定位准确，但权限机制需要另查。checksum-content 保持相同路径和 open 成功结果；RunDelta 的记录存储摘要不校验被测 source.tar 内容，不能解决这个案例。

在本次核对范围内没有确认新的事实误报；这不等于全事件均经人工审计。没有根据本批结果改排序或增加内容采集功能，也没有把这批改成新的排序训练集。

## 复现与证据

需 Linux 普通用户（tar chmod 试验不可用 root 绕过权限）、OpenSSL、Python、tar、Git、sha256sum 和可用 strace。脚本不联网，不装软件，只在新建 target/holdout-* 中生成测试证书/私钥、Git 供体/克隆、归档与记录。夹具仓库有自己的测试提交，不使用产品仓库身份。

```bash
cargo build --offline
python3 tests/check_regression_freeze.py
RUNDELTA_STRACE="$PWD/target/tools/strace-6.16/src/strace" python3 examples/holdout_cases.py
```

实际运行摘要：

```text
FINISH openssl-trust: top5=True, rank=3, changes=3, control=0, diff=1
FINISH python-shadow: top5=True, rank=2, changes=48, control=0, diff=1
FINISH tar-permission: top5=True, rank=2, changes=2, control=0, diff=1
FINISH git-alternates: top5=True, rank=2, changes=17, control=0, diff=1
FINISH checksum-content: top5=False, rank=None, changes=1, control=0, diff=1
```

本次 `$WORK` 为 `target/holdout-8flion4l`。证据入口：

- [执行前计划](HOLDOUT_PLAN.md)、[复现脚本](../examples/holdout_cases.py)、[脱敏归档结果/源码与计划摘要](HOLDOUT_RESULTS.json)。
- 完整命令及输出（原本地路径 `../target/holdout-8flion4l/commands.json`）、本次 manifest（原本地路径 `../target/holdout-8flion4l/manifest.json`）、执行日志（原本地路径 `../target/holdout-audit/run.log`）。
- 原样报告：OpenSSL（原本地路径 `../target/holdout-8flion4l/openssl-trust-comparison.txt`）、Python（原本地路径 `../target/holdout-8flion4l/python-shadow-comparison.txt`）、tar（原本地路径 `../target/holdout-8flion4l/tar-permission-comparison.txt`）、Git（原本地路径 `../target/holdout-8flion4l/git-alternates-comparison.txt`）、checksum（原本地路径 `../target/holdout-8flion4l/checksum-content-comparison.txt`）。同目录包含 JSON、正常对照及 stdout/stderr。
- 每条差异附带原始文件行号；运行 ID 见结果 JSON，追踪位于 `records/runs/<run-id>/raw/trace.log`。

例如只读复查：

```bash
target/debug/rundelta --storage target/holdout-8flion4l/records diff tar-permission-good tar-permission-bad --json
# 退出 1；第 2 条为 output/README.md success → EACCES
```

target 内含临时测试密钥与大体积追踪，保持 Git 忽略；清理 target 会使本地证据链接失效。项目提交保存脚本、计划、冻结摘要和结果，不将临时密钥或构建目录纳入 Git。重跑的路径、对象 ID 和名次可能随环境变化；不同结果应报告，不修改预期以隐藏失败。

Cargo 的共享 FS 场景见 [可靠性不足时浏览观察差异](INCOMPLETE_CAPTURE.md)。其退出码 3 已作为正式预期行为记录，本批全为退出码 1 不会取代该契约。

## 基线检查

本轮默认单元测试 23 通过；显式平台集成 2 项通过（含可靠性脚本 8 项）；原专项审计 FINDINGS: 0；fmt/clippy 通过。固定旧五例重新采集到 `target/field-cases-53wi81bh`，冻结审计全部通过，Cargo 保持唯一共享 FS 警告和 diff 3。冻结的脚本、断言与历史成绩 SHA-256 一致。

日志：单元测试（原本地路径 `../target/holdout-audit/cargo-test.txt`）、平台集成（原本地路径 `../target/holdout-audit/acceptance.txt`）、专项审计（原本地路径 `../target/holdout-audit/audit.txt`）、固定五例复测（原本地路径 `../target/holdout-audit/regression-run.log`）、固定五例审计（原本地路径 `../target/holdout-audit/regression-audit.txt`）。

产品源码本轮未改。首个 Git 基线将包含源码、Cargo.lock、测试、示例、README、历史验收、冻结清单与本批预登记/结果；排除 target 中的追踪、临时私钥和构建物。原用户暂存的 `.idea/.gitignore` 与其他 IDE 私有配置不纳入产品基线。首个基线使用用户关联的 GitHub 账户身份，仅通过本次提交的进程环境传入，不修改全局 Git 配置。
