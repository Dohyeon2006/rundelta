# 事前登记：pip #12996

检查日期：2026-09-20。Issue：[pip user config is invisible when PIP_CONFIG_FILE is set](https://github.com/pypa/pip/issues/12996)。GitHub issue API 状态 **open**；标签 `type: bug`、`S: needs triage`。只读正文、状态、标签及许可证/贡献文档；未读取评论、PR 或提交讨论。

## 正文条件与范围

正文给出 Linux、pip 23.2.1–24.2、Python 3.10–3.12，以及第二种直接复现：用户配置中有 `install.constraint`，设置 PIP_CONFIG_FILE 指向另一现存文件后，同一 `pip config get install.constraint` 返回缺少该键；将变量置空则返回原值。

本例固定 **pip 24.2、CPython 3.12.11、Linux x86_64**，使用独立 HOME 和两个事前创建的配置文件。用户配置声明 constraint 与 require-virtualenv；覆盖配置采用正文列出的 install 设置，不含 constraint。使用字面占位值 `constraints.txt`，查询配置不读约束文件，不进行安装或网络请求。两个文件在所有运行中均存在且内容不变。

这是尝试独立复现正文的**配置查询症状 2**，不是注入缺失依赖，也不代表已经复现一次实际包构建失败。若症状不出现，原样记录“不复现”。即使复现，也不自动认定为 pip 实现缺陷或证明其配置设计意图。

## 唯一对照条件

全部运行使用完全相同 argv，固定同一 CWD、解释器、pip 安装、HOME、缓存设置和配置文件：

```bash
"$PIP_VENV/bin/pip" config get install.constraint
```

唯一改变的环境键：**PIP_CONFIG_FILE**。

- good / repeat：空字符串。
- bad / bad-repeat：`$CASE/override.conf`。

命令不增加 `--isolated`，否则会改变待检验的配置加载语义。RunDelta 不选择任何启动环境变量；评价自动采集的文件访问证据，不将预先选定变量本身作为排名命中。

## 预登记线索与门禁

关键线索有两个，原始 JSON `changes` 的下标加一就是排名：

1. `$CASE/override.conf` 的成功 open/openat 在失败运行中新增。
2. `$CASE/home/.config/pip/pip.conf` 的成功 open/openat 在失败运行中消失。

严格前五命中：至少一个上述**精确路径、文件访问类别、操作和 success 结果变化**在第 1–5 条。仅退出状态、无关 Python 库或另一个 pip.conf 路径不算。分别记录两条线索的实际排名，不事后改集合。

严格有效对照：两个 good 返回 0 并输出 `constraints.txt`；两个 bad 返回 1 且报告 `No such key - install.constraint`；源码/安装文件与配置内容不变，argv/CWD 相等，全环境仅上述键不同。记录正常→正常、正常→异常、异常→异常的 JSON/text，保留全部私人原始证据。

采集成功与诊断命中分别评价：必须显示 capture、storage 和 diff 退出码。若 capture 不完整，不能以线索命中替代完整性；告警全量计数并分类，未知保持未知。正常重复若有差异，记录噪声数量，不过滤后重新计算排名。

## 能与不能观察

RunDelta 可以观察实际执行、配置文件打开和访问结果、进程退出；可以证明本次记录中哪些路径被观察到。它不能读取配置值或证明 pip 内部优先级算法，不能以“未观察到 open”证明所有形式的读取绝不存在，也不能从两次记录证明第三方缺陷根因。

pip 使用 MIT 许可证；已读取 24.2 的 LICENSE.txt、.github/CONTRIBUTING.md 及其贡献入门说明。只使用未修改的官方发布 wheel，所有安装与原始记录留在独立临时目录。复现脚本和结果另存于本目录的父目录；本文件在首次目标运行前冻结摘要。
