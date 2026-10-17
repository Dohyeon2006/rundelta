# bracket-lib #376：原依赖配置阶段的重放

**expat-sys 2.1.6 的原始 CMake 配置失败阶段复现；对照有效；新 CMake 执行路径排第 3。** 未构建完整 hello_bterm/Cargo 依赖图，因此原 issue 只能标为“部分复现：依赖配置阶段”。没有删除头文件或修改第三方源码制造相似症状。事前 [CASE.md](CASE.md) 未改。

## 已知信息、控制与独立事实

- 正文已知：Fedora 上 expat-sys 2.1.6 的 CMake 配置失败，报旧兼容模式不再支持。
- 实验控制检查：同一未修改 crate 源码、同一配置 argv/CWD、相同系统编译器和空 build 初态；唯一环境差别是 PATH 首项的 CMake 提供者。工具安装、源码摘要前后不变；HOME/cache/tmp 无新增输入。每轮输出分别归档，无旧 CMakeCache 跨轮复用。
- RunDelta 独立事实：实际成功执行路径由 CMake 3.31.6 切到 4.0.3；目标/采集器退出 0→1。失败侧的新执行路径在第 3 条，旧路径消失在第 352 条。
- 目标输出另行观察：正常两次配置成功并生成 Makefile；失败两次出现 `cmake_minimum_required` 的兼容模式错误。RunDelta 不解释 CMake 政策，也没有读文件内容快照。
- 合理推断：本例的失败与唯一改变的 CMake 提供者相关，可定位到工具链切换；不能仅据路径或两侧退出状态证明内部根因。
- 未证明：完整 hello_bterm 构建是否成功、整个 Cargo 解析图、所有 bracket-lib 版本或当前上游分支仍有同一故障。

## 实际运行与可靠性

| 运行 | 目标 / 采集器退出 | 事件数 | capture | storage | 告警 |
|---|---|---:|---|---|---:|
| good | 0 / 0 | 16,188 | partial | verified | 1 |
| repeat | 0 / 0 | 16,188 | partial | verified | 1 |
| bad | 1 / 1 | 2,560 | partial | verified | 1 |
| bad-repeat | 1 / 1 | 2,560 | partial | verified | 1 |

四次记录均 completed、无终止信号。每次唯一告警关联 root 的 `access`：相对操作数 `cmake`，X_OK，ENOENT，保存为 `relative:cmake [cwd unresolved]`，本次局部证据 11–11。这是没有当次绝对路径证据的相对访问；不能从最后见到的 CWD 拼造物理路径。没有未知 syscall 或额外生命周期/存储告警。

RunDelta 保留 **capture=incomplete / storage=verified / diff=3**，文本明确显示 `RELIABILITY NOT ESTABLISHED`。本例不豁免告警，不把 completed 或预登记线索命中称作完整采集。绝对 execve 线索有其自身直接证据，但仍不能升级整次记录的完整性。

| 比较 | 观察差异数 | diff 退出码 | 组成 |
|---|---:|---:|---|
| good→repeat | **2,262** | 3 | 新增文件访问 1,131、消失 1,131 |
| good→bad | **2,072** | 3 | 退出 1、成功执行变化 2、执行尝试变化 2、新增文件访问 438、消失 1,629 |
| bad→bad-repeat | **242** | 3 | 新增文件访问 121、消失 121 |

正常重复中没有执行路径变化；大量构建工作路径差异仍在完整 JSON 中，未从计数中删除。它们是本次对照中的观察噪声，不证明对其他任务均无意义。

## 正常→异常的原始前五

表中的 `V4` 表示 `${WORK}/tools/cmake-4.0.3/lib/python3.12/site-packages/cmake/data/bin`，`B` 表示 `${CASE}/build`，只是脱敏显示别名。

| 排名 | 类别 | 路径 / 操作 | 左→右 |
|---:|---|---|---|
| 1 | Exit status | — | code 0→1 |
| 2 | Execution attempt changes | `V4/as` / execve | 未观察→ENOENT |
| **3** | **Executable changes** | **`V4/cmake` / execve** | **未观察→success** |
| 4 | New file access | `B/CMakeFiles/4.0.3/CMakeCCompiler.cmake` / access | 未观察→ENOENT, success |
| 5 | New file access | `B/CMakeFiles/4.0.3/CMakeCXXCompiler.cmake` / access | 未观察→ENOENT, success |

预登记的严格执行线索排第 3，判定命中。第 2 条是有误导风险的搜索尝试噪声：同一失败运行还观察到 **4 次 `/usr/bin/as` 成功执行**，不能把这个 ENOENT 说成汇编器缺失。第 4/5 条同一路径既出现失败又成功，不宜单独诊断为损坏文件。没有改动排序来改善这些现象。

可定位程度是“工具链提供者变化”，不是“RunDelta 找到了兼容政策根因”。这是本次操作者依据证据的判断，没有外部用户盲测。

## 复现、证据与贡献边界

父目录 [reproduce.py](../reproduce.py) 固定下载哈希，执行同一 CMake 命令；[CASE.md](CASE.md) 写明所有不变参数与唯一 PATH 条件。仅执行配置，不继续编译应用。

[summary.json](summary.json) 保留三类比较的前五、告警、可靠性和原 JSON 摘要；[evidence.json](evidence.json) 提供两个 CMake 路径的直接 execve 证据，以及成功系统汇编器的反例引用。只包含标准化字段和局部行号，没有 raw strace、第三方源码或构建产物。

**贡献判断**：可作为后续独立依赖/工具链兼容验证的起点；须先在完整应用上验证并检查当前依赖状态，才值得准备贡献。没有确认新的 RunDelta 采集/解析缺陷；本次 CWD 不确定性和 diff=3 与既有语义一致。
