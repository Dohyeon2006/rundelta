# spades-1339：实测结果

## 复现判定与已知信息

正文是 SPAdes 4.0.0 Linux 发布包、Python 3.10.12、Ubuntu 22.04.4，安装放入带空格目录并在 bin 执行自带测试。它没有将安装路径与输出路径分开控制。本次使用官方同版本发布包与自带测试数据，源文件保持原样；实际系统是 Fedora 44。为保持第三方目录不被运行输出污染，在外部分别建立等价空格条件的 runtime CWD，而非在发布包 bin 内生成输出。

## 保留的所有尝试

| 阶段 | Python/路径条件 | 两次目标退出 | diff/条数 | 判定 |
|---|---|---|---|---|
| 首次 | 3.10.12；同一无空格 CWD | 127 / 127 | 1 / 3 | Python 缺 libcrypt.so.1；没有正常运行，不计原问题 |
| 第二次 | 3.10.19；只有安装路径含空格；CWD 不变 | 0 / 0 | 3 / 6287 | 未复现失败，是成功/成功比较，不称正常/异常 |
| 最终 | 3.10.12 + 本地 libcrypt；安装及工作/输出路径一起变化 | 0 / 4 | 3 / 3659 | 重现相同转换错误；两变量混杂仍保留 |

第二次成功运行和最终失败之间也改变了解释器 patch/兼容库，所以不能仅依这两个阶段断言输出路径是充分根因；下一阶段需固定解释器做正交控制。没有删除或覆盖前两阶段记录。

## RunDelta 独立事实

最终两侧都是已完成记录但采集 partial，各有共享文件系统状态警告；存储 verified。tracer 与目标退出分别为 0/4，diff=3；不能将 4 误称采集器异常，也不能将 diff=3 误称损坏。

第 2 条显示初始 CWD 从无空格变为有空格。第 3 条是入口参数，第 4/5 条是新安装位置的成功 exec。第 4 条的原始证据（bad-v3 第 529–531 行）显示 hammer 成功执行，配置路径作为**一个完整参数**传入，没有在该边界按空格拆开。这不支持最初可能的参数拆分假设。目标输出包含：

```text
Exception caught conversion of data to type "std::filesystem::__cxx11::path" failed
```

这条消息来自目标程序，不是 RunDelta 的根因解释。

## 推断、噪声、遗漏

第 2 条有助于检查空格路径条件，但没有给出预登记的错误路径访问/参数拆分证据。安装路径变化本身不计成功，不能事后把较弱的线索升格。完整参数传递也不能排除后续配置内容解析问题；本轮没读取/分析其内容来证明根因。

3418 条消失的文件访问包含失败后不再发生的构建/装配活动；成功/成功的前一阶段亦有 6287 条差异。它们是实际观察变化，不能一律称误报。RunDelta 未捕获内部转换语义，未证明内容因果；第 4 名“执行成功”若被读成工具有问题会误导。

严格命中：**否**；部分范围帮助：**第 2 条**。定位：能缩小到路径条件，不足以定位内部失败位置。值得进一步独立验证根因；本轮不准备补丁、不对外联系。

## 产品原顺序前五

1. **Exit status** — `(metadata)`

   左：`code=Some(0), signal=None`；右：`code=Some(4), signal=None`。

2. **Initial working directory** — `(metadata)`

   左：`${WORK}/spades/runtime/bin`；右：`${WORK}/spades/runtime with spaces/bin`。

3. **Command arguments** — `(metadata)`

   左：`["${WORK}/spades/install/SPAdes-4.0.0-Linux/bin/spades.py", "--test", "--careful"]`；右：`["${WORK}/spades/install with spaces/SPAdes-4.0.0-Linux/bin/spades.py", "--test", "--careful"]`。

4. **Executable changes** — `${WORK}/spades/install with spaces/SPAdes-4.0.0-Linux/bin/spades-hammer (execve)`

   左：``；右：`success`。

5. **Executable changes** — `${WORK}/spades/install with spaces/SPAdes-4.0.0-Linux/bin/spades.py (execve)`

   左：``；右：`success`。

## 数量与证据

| 类别 | 数量 |
|---|---:|
| Exit status | 1 |
| Initial working directory | 1 |
| Command arguments | 1 |
| Executable changes | 9 |
| New file access | 226 |
| Execution attempt changes | 3 |
| Removed file access | 3418 |

共 3659 条（含折叠细节）；150 条带 detail_group 标记，不等于从原始数据删除。

完整的脱敏前五及所选后排证据位置见 [summary.json](summary.json)；少量解码/脱敏 syscall 证据见 [evidence-excerpts.json](evidence-excerpts.json)。复现命令见 [REPRODUCE.md](../REPRODUCE.md)。原始日志没有随文档公开；证据行号仅指向本次隔离记录。

## 独立重放中的额外不确定性

SPAdes 的第二阶段成功运行 `bad-v2` 在重放中新增 `unfinished call at end of trace` 告警（原始第 24101 行）。实际追踪是 `???( <unfinished ...>`，同一 PID 第 24107 行正常退出；本轮没有识别这个 syscall，也未证明它是 RunDelta 解析器缺陷。目标仍退出 0，存储仍 verified，采集仍 incomplete，diff 仍为 3。全部告警及两行脱敏证据保留在 [REPLAY.json](../REPLAY.json)。新增的一条 Capture completeness 差异解释了总数 6287→6288，不能仅归为无害临时文件噪声。后续应独立复现并分类这条未知调用，不能删除告警使报告“完整”。
