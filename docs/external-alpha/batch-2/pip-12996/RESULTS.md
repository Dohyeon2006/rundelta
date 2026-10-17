# pip #12996：配置查询症状复现

**原正文的症状 2 在 Linux 上独立复现；有效对照；关键事实排第 2 和第 3。** 本例运行 pip 的配置查询命令，不是一次包安装或构建成功率测试。事前登记 [CASE.md](CASE.md) 的内容和摘要未改。

## 已知信息、控制与独立事实

- Issue 正文已知：PIP_CONFIG_FILE 指向现存配置文件时，用户配置中的 `install.constraint` 查询失败；空字符串时可查询。
- 实验控制检查：同一 pip 24.2、Python 3.12.11、完整 argv、CWD、HOME 和两个配置文件；只改变 PIP_CONFIG_FILE。配置/安装文件摘要前后相同，HOME 除事前配置外没有新增文件，cache/tmp 为空。没有为 RunDelta 选择启动环境变量。
- RunDelta 独立事实：失败运行新增成功打开覆盖配置；正常运行中成功打开的用户配置在失败记录中不再被观察到。目标和采集器退出码都从 0 变为 1。
- 目标输出另行观察：正常两次打印 `constraints.txt`；失败两次报 `ERROR: No such key - install.constraint`。这段输出不属于 RunDelta diff 自动解释。
- 推断：配置加载范围与该查询失败有关，路径线索可直接指向本例的唯一条件。
- 不能证明：pip 内部优先级实现、设计意图、任意安装失败或上游一定需要代码修复。RunDelta 未记录配置文件内容；文件内容一致来自独立实验控制检查。

## 实际运行与报告

| 运行 | 目标 / 采集器退出 | 事件数 | capture | storage | RunDelta 告警 |
|---|---|---:|---|---|---:|
| good | 0 / 0 | 491 | complete_for_supported_events | verified | 0 |
| repeat | 0 / 0 | 491 | complete_for_supported_events | verified | 0 |
| bad | 1 / 1 | 490 | complete_for_supported_events | verified | 0 |
| bad-repeat | 1 / 1 | 490 | complete_for_supported_events | verified | 0 |

记录生命周期均为 completed，没有目标/采集器终止信号。正常重复与异常重复各 **0 条差异、diff=0**；正常→异常 **3 条差异、diff=1**。三种比较的文本/JSON 退出码一致。

全部差异仅三条，没有补造第 4/5 条：

| 排名 | 类别 | 路径 / 操作 | 左→右 |
|---:|---|---|---|
| 1 | Exit status | — | code 0→1 |
| 2 | New file access | `${CASE}/override.conf` / openat | 未观察→success |
| 3 | Removed file access | `${CASE}/home/.config/pip/pip.conf` / openat | success→未观察 |

两条事前登记线索均命中。第 2 条只是“观察到打开另一配置”的事实，第 3 条是“本次支持的观察中不再出现用户配置”的事实；并非文件已被删除。没有观察到的 syscall、文件内容及配置覆盖机制不能靠 diff 自动证明。

本例未发现前五中的无关噪声或未知事件；正常重复为零，仅证明这组受支持的聚合观察相同，不能推广为所有运行条件一致。操作者可据此定位到配置选择层；没有独立用户盲测。

## 复现与证据

使用父目录 [reproduce.py](../reproduce.py) 固定安装版本、建立隔离环境并执行四次同命令采集；具体命令和唯一变化见事前 CASE。父目录 README 给出完整执行入口。

[summary.json](summary.json) 保留四次状态、三种比较、全部前五条目、未改写的产品排名以及完整私人 JSON 的 SHA-256。[evidence.json](evidence.json) 保留关键事件字段、root 角色和本次 raw 文件/开始/结束行号。分析器核对原始 syscall 的绝对操作数和成功返回；公开文件不包含 raw 行或完整记录。行号仅用于本次私有记录回溯，不能要求重采集时相等。

**贡献判断**：值得做独立的配置语义/文档一致性确认，但当前证据不足以认定 pip 代码缺陷或直接准备修复 PR。本阶段没有读取评论或进行任何对外互动。
