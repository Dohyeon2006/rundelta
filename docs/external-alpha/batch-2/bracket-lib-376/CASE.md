# 事前登记：bracket-lib #376 的依赖配置阶段

检查日期：2026-09-20。Issue：[CMake error when compiling hello_bterm](https://github.com/amethyst/bracket-lib/issues/376)。GitHub issue API 状态 **open**，标签为空。只读正文、状态、标签、LICENSE、贡献文件所在目录及复现必要文件；未读取评论、PR、提交讨论或维护者讨论中的分析。

## 正文条件与范围

正文报告 Fedora 44 上 hello_bterm/bracket-lib 构建失败，给出 `expat-sys 2.1.6` 的 CMake 配置调用及错误：CMake 不再支持低于 3.5 的兼容模式。正文列出的 bracket-lib 0.8.1/0.8.7 都失败。

本例固定官方 **expat-sys 2.1.6 crate 中未经修改的 expat 源码**，重放其失败的 CMake 配置阶段。good 选择 **CMake 3.31.6**，bad 选择 **CMake 4.0.3**；固定系统 GCC、配置参数和输出目录。这里的唯一工具链条件是 PATH 中 CMake 提供者变化；目标项目/依赖源码版本不变。

**不声称完整复现 hello_bterm 或整个 Cargo 依赖图。** 若出现正文相同的兼容性错误，只能称“独立复现原 issue 指明的依赖配置失败阶段”；这是原始依赖上的缩小重放，不是删除头文件等人为相似故障。若完整应用级验证是成功标准，本例只能算部分复现。

## 唯一对照条件

全部运行使用相同 argv、同一 CWD、源码树和构建输出位置：

```bash
cmake "$CASE/source/expat-sys-2.1.6/expat" \
  -B "$CASE/build" \
  -DBUILD_shared=OFF -DBUILD_tools=OFF -DBUILD_examples=OFF -DBUILD_tests=OFF \
  "-DCMAKE_INSTALL_PREFIX=$CASE/install" \
  "-DCMAKE_C_FLAGS= -ffunction-sections -fdata-sections -fPIC -m64 -w" \
  -DCMAKE_C_COMPILER=/usr/bin/cc \
  "-DCMAKE_CXX_FLAGS= -ffunction-sections -fdata-sections -fPIC -m64 -w" \
  -DCMAKE_CXX_COMPILER=/usr/bin/c++ \
  "-DCMAKE_ASM_FLAGS= -ffunction-sections -fdata-sections -fPIC -m64 -w" \
  -DCMAKE_ASM_COMPILER=/usr/bin/cc -DCMAKE_BUILD_TYPE=Debug
```

唯一改变的环境键：**PATH**，其首项分别是 CMake 3.31.6 或 4.0.3 的原生 `data/bin` 目录，后缀固定 `/usr/bin:/bin`。不更改 CMAKE_POLICY_VERSION_MINIMUM、源码、选项或入口命令。RunDelta 不选择任何启动环境变量。

每次从同一个空 build 目录开始。结束后将本次输出移入独立归档，重新创建同名空目录，不在 good→bad 之间沿用缓存；这是所有 phase 一致执行的复位步骤。原始记录和归档从不覆盖。工具安装和源码仅进行读取，不修改第三方项目。

## 预登记线索与门禁

关键线索：失败侧 **CMake 4.0.3 的真实绝对可执行文件路径**在 `Executable changes` / `execve` 中新增、结果 success；正常侧的 CMake 3.31.6 路径消失作为配套证据。不能用 CMake 配置文件、退出状态或动态构建文件替代此线索。

严格前五命中：失败侧 CMake 执行路径条目排第 1–5，且证据能回到本次相同 PID 的 execve 原始行。所有排名用完整 JSON 原顺序，不改排序或折叠后再计数。

严格有效对照：good/repeat 配置成功且生成 Makefile；bad/bad-repeat 非零退出，stderr 含正文的 CMake 兼容性错误。全部 argv/CWD 相等、全环境差别恰为 PATH，源码和工具安装摘要不变。若 good 也失败，则不能声称有正常/异常有效对照，保留两侧真实结果。

记录正常→正常、正常→异常、异常→异常，分别报告 capture、storage、diff 退出码、前五和告警。当前 CWD 不确定性、未知 syscall 或不完整采集不豁免；通过目标症状不等于通过完整性要求。

## 能与不能观察

RunDelta 应能提供实际执行 CMake 路径的事实线索，以及配置期间访问/退出变化。它不能读取 CMakeLists 的内容语义、推导政策兼容规则，也不能仅凭可执行路径证明根因；兼容性错误来自另行保存的目标 stderr，必须与 RunDelta 事实分开。

bracket-lib LICENSE 为 MIT，根目录和 .github 中未发现 CONTRIBUTING；CMake 使用 BSD 3-clause，已读 LICENSE.rst 和 CONTRIBUTING.rst。本例不执行其中任何贡献操作。expat-sys 的许可证与最小 CMake 文件将在下载后、首次运行前检查。只保留脱敏报告，不将 crate、工具 wheel、构建目录或原始追踪复制到仓库；本文件在首次目标运行前冻结摘要。
