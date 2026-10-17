# pybind11-3445：实测结果

## 复现判定与已知信息

正文报告 Python 3.9 虚拟环境内 pip 安装 pybind11 2.8.1 后，CMake 3.21.1 无法发现该包。使用正文三行工程，在未设置前缀时重现相同包发现错误；明确设置包的 CMake 目录时成功。这是功能症状复现，不是报告者历史运行的恢复。未修改第三方源码。清空构建状态通过移动本次构建目录实现，记录不删除。

## RunDelta 独立事实

目标和 tracer 退出码均由 0 变 1。第 2 条显示选定 `CMAKE_PREFIX_PATH` 从安装目录变为未设置。两侧采集完整且存储通过验证，diff=1。配置文件成功 `access` 消失在第 110 条、成功 `openat` 消失在第 230 条；第 510 条起可见部分失败搜索。真正的文件访问线索本身没有进入前五。

## 推断、噪声、遗漏

前缀配置是有针对性的排查线索，但因为操作者预先选了该环境变量，不能把结果推广为自动识别解释器/环境漂移。前五的第 3–5 条是构建日志或随机探测产物，无法解释缺包。635 条观察差异中含提前退出导致的后续活动消失；未对每一条作根因标注，也未将其全部视为误报。文件内容未被采集；没有证据表明 pybind11 上游实现有缺陷。

严格命中：**是，第 2 条**。定位：能检查前缀与包位置，不能由此证明唯一根因。

## 产品原顺序前五

1. **Exit status** — `(metadata)`

   左：`code=Some(0), signal=None`；右：`code=Some(1), signal=None`。

2. **Selected startup environment** — `CMAKE_PREFIX_PATH`

   左：`value="${WORK}/pybind/venv/lib/python3.9/site-packages/pybind11/share/cmake/pybind11"`；右：`(unset)`。

3. **New file access** — `${WORK}/pybind/build/CMakeFiles/CMakeOutput.log (access)`

   左：``；右：`success`。

4. **New file access** — `${WORK}/pybind/build/CMakeFiles/CMakeTmp/cmTC_ad1fd (access)`

   左：``；右：`success`。

5. **New file access** — `${WORK}/pybind/build/CMakeFiles/CMakeTmp/cmTC_bfbb3 (access)`

   左：``；右：`success`。

## 数量与证据

| 类别 | 数量 |
|---|---:|
| Exit status | 1 |
| Selected startup environment | 1 |
| New file access | 119 |
| Executable changes | 3 |
| Removed file access | 511 |

共 635 条（含折叠细节）；126 条带 detail_group 标记，不等于从原始数据删除。

完整的脱敏前五及所选后排证据位置见 [summary.json](summary.json)；少量解码/脱敏 syscall 证据见 [evidence-excerpts.json](evidence-excerpts.json)。复现命令见 [REPRODUCE.md](../REPRODUCE.md)。原始日志没有随文档公开；证据行号仅指向本次隔离记录。
