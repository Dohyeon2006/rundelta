# Cargo：可靠性不足时浏览已观察到的差异

> 历史快照（保存记录合同）：本文保留旧版全局 `CLONE_FS` 回退模型产生的既有记录、warning 和退出码，用于证明历史证据没有被新 parser 追溯改写。它不是当前版本重新采集 Cargo 的预期。当前权威行为见 [FsContext 模型](FS_CONTEXT_MODEL.md)；历史冻结与新鲜语义的两个独立入口见 [五案例审计分层](FIELD_AUDIT_LAYERS.md)。

Cargo/Rust 构建会创建共享文件系统状态的进程/线程。生成这些保存记录的旧版解析器检测到 CLONE_FS 等共享状态时，会禁用整次运行基于进程继承的 CWD 回退推断并将采集标记为不完整；可从 strace 明确目录注释解释的访问仍被保留。对本文的历史记录，**diff 退出码 3 是正确行为**，不能为展示比较结果而改成 0 或 1。当前解析器改为局部 FsContext，见上方链接。

## 真实示例

固定回归集的 cargo-linker：正常侧用 cc 构建 RunDelta，异常侧将 CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER 指向不存在的文件。它同时展示三个独立事实：

- 观察差异：有，异常侧尝试执行 missing-linker，返回 ENOENT。
- 采集完整性：两侧 incomplete，共享 FS 的完整 CWD 语义未覆盖。
- 存储完整性：两侧 verified，已保存的 metadata/events 与完成标记一致。

以下命令仅在原历史目录仍被保存时可查看既有记录。目录缺失意味着这份历史材料当前无法核验，不能用当前 parser 重跑来补造；新采集必须使用独立目录和新鲜语义审计。

These commands require the preserved historical directory. Missing records remain unavailable; a current-parser capture is new evidence and must use a separate directory and the fresh audit, not recreate this historical snapshot.

```bash
STORE="$PWD/target/field-cases-aden7dm4/records"
target/debug/rundelta --storage "$STORE" diff cargo-linker-good cargo-linker-bad
# 退出码为 3，不是 1。
```

实际文本关键内容：

```text
Observed differences: yes
Capture completeness: left=incomplete, right=incomplete
Storage integrity: left=verified, right=verified
Exit code: 3
RELIABILITY NOT ESTABLISHED: comparison cannot establish a complete, verified result.

Execution attempt changes
  .../cargo-linker/missing-linker (execve)
  (not observed) → ENOENT
```

warning 为 `shared filesystem state or root/namespace changes observed; cwd fallback disabled`。完整报告仍有具体证据位置。可以检查 missing-linker 路径与启动配置，但不能由未观察到的事件断言它没有发生，也不能将这些观察称为完整根因证明。

## 自动化中保留退出码

以下示例不会因退出码 3 丢弃 JSON，也不会将 3 改写成成功：

```bash
if target/debug/rundelta --storage "$STORE" diff cargo-linker-good cargo-linker-bad --json > cargo-report.json; then
    diff_status=0
else
    diff_status=$?
fi
printf 'RunDelta exit code: %s\n' "$diff_status"
PYTHONDONTWRITEBYTECODE=1 python3 -B - <<'PY'
import json
r = json.load(open('cargo-report.json'))
print(r['observed_differences'], r['reliability'], r['exit_code'])
for change in r['changes'][:5]:
    print(change['category'], change['subject'], change['left'], change['right'])
PY
# 调用方按自身策略处理 diff_status；严格门禁应保留“不完整”状态。
```

JSON 关键字段为：

```json
{
  "schema_version": 4,
  "observed_differences": true,
  "reliability": {
    "left": {"capture": "incomplete", "storage": "verified"},
    "right": {"capture": "incomplete", "storage": "verified"}
  },
  "exit_code": 3
}
```

`record` 的目标退出状态与 `diff` 可靠性退出码不同：本例正常目标返回 0、错误链接器构建返回 101；记录器写完记录也不能消除采集模型边界。采集器自身失败须另看 metadata 的 collector 状态和 capture_failed，不能仅根据 diff 3 认定追踪器故障。

完整无差异为 0，完整有差异为 1，损坏/工具错误为 2，不完整/存储未经验证为 3。即便不完整记录自比较没有观察差异，也仍返回 3。存储 verified 只验证记录文件，不证明被测输入文件内容相同，也不覆盖全部系统调用。

不要用当前版本“重新生成”本文中的历史 warning 或证据坐标。`tests/field_report_audit.py` 只对冻结历史目录保留该 warning/返回码检查；当前脚本新生成的目录必须由 `tests/fresh_field_audit.py` 验证。新鲜 Cargo 合同要求已证明的 `CLONE_FS` 由局部 FsContext 建模，预登记样例为零 warning、capture complete、storage verified，正常→异常 diff 为 1；若出现新的不确定性则仍应失败闭合，而不是为了得到 0/1 删除 warning。具体命令见 [固定回归集](REGRESSION_SET.md)。
