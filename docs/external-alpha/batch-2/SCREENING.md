# 候选筛选记录

日期：2026-09-20。筛选使用 GitHub issue API/仅标题正文的 issue 搜索，限制公开、open；只读取正文、状态、标签、许可证/贡献文档与复现必要源码。未请求 issue 评论、PR 内容、commit discussion 或维护者讨论。检索所得推断与实际采集分开。

| Issue | 当日状态/标签 | 本机同入口对照评估 | 决定 |
|---|---|---|---|
| [pypa/pip #12996](https://github.com/pypa/pip/issues/12996) | open；type: bug、S: needs triage | 正文给 Linux 配置查询命令；固定 pip/Python/文件，只切换 PIP_CONFIG_FILE | 入选，正文症状 2 完整复现；没有把配置查询称为包构建 |
| [amethyst/bracket-lib #376](https://github.com/amethyst/bracket-lib/issues/376) | open；无标签 | 正文给出失败的 expat-sys CMake 配置阶段；固定原始依赖和 argv，PATH 选择 CMake3/4 | 入选，但只声称依赖配置阶段部分复现 |
| [astral-sh/uv #21047](https://github.com/astral-sh/uv/issues/21047) | open；needs-mre | 正文报告 macOS pyenv shim 失败；未先证明 Linux 下能建立相同实际解释器发现条件 | 未执行；不将移植的人工 shim 当作原 Linux issue 复现 |
| [rust-lang/cargo #4350](https://github.com/rust-lang/cargo/issues/4350) | open；A-diagnostics、Command-install、S-needs-design | 需要可写且 noexec 的临时文件系统；本机所检查的候选挂载不满足，无合适的无特权条件 | 未执行；不挂载文件系统，也不把 chmod 故障替换为 noexec 原问题 |
| [mesonbuild/meson #15740](https://github.com/mesonbuild/meson/issues/15740) | open；regression、language:cython | 正文为 conda/system Python 选择与 PKG_CONFIG_PATH 差异；本机缺少对应 Python 开发文件/双环境条件 | 未执行；不伪造开发文件，不比较两个 Meson 版本或不同入口来凑对照 |

未入选表示“在本机已具备条件和本阶段约束下，不适合直接进行 RunDelta 对照”，不代表这些公开 issue 在所有 Linux 环境都无法复现。没有隐藏执行失败；以上三例没有进入目标采集，不能计入命中率或复现失败率。最多三例不是必须三例。

两个入选 issue 在采集后再次查询仍为 open，且在 `is:public is:issue is:open -linked:pr` 搜索中返回。该检索只能排查索引到的关联 PR，不能证明不存在未关联/未索引的修复。状态、标题、标签和正文摘要保存在 [provenance.json](provenance.json)，没有复制评论或完整 issue 正文。

## 仓库材料检查与使用方式

- pip：读取 24.2 的 MIT `LICENSE.txt`、`.github/CONTRIBUTING.md` 与其入门文档；使用未修改官方 wheel。目标文件由本实验创建，只有两个简短配置文件；不更改 pip 实现。
- bracket-lib：读取公开主分支 MIT LICENSE，检查根目录与 `.github` 未发现 CONTRIBUTING。没有克隆完整应用进行构建，因此不声称完整应用复现。
- 实际依赖：下载并核对官方 expat-sys 2.1.6 crate；读取 Cargo.toml、expat/COPYING 和复现必要 CMake 文件。使用原始源码，未打补丁。MIT 许可证及 CMake 文件原有 BSD 构建系统声明保留在私人源码中，不复制到本仓库。
- CMake：读取 BSD 3-clause LICENSE.rst 和 CONTRIBUTING.rst；使用官方发布的 3.31.6/4.0.3 wheel 中的原生二进制。没有执行贡献文档中的 Fork、Git 配置或提交操作。

这里是只读研究和独立验证，不是上游根因审查或修复贡献。
