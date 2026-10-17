# 第二批五例：执行前登记

本计划先于新案例的任何 RunDelta record 写入。当前排序、解析与展示代码冻结；这批结果只用于评估，不据此修改规则。原有五例成为固定回归集，见 REGRESSION_SET.md。新例使用实际 OpenSSL、Python、tar、Git、sha256sum；全部为隔离目录中的真实工具受控故障，不声称来自外部用户事故。

## 预期关键差异（事先确定）

| ID | 正常流程 → 注入变化 | 预期关键观察 | 事先预期可定位程度 |
|---|---|---|---|
| openssl-trust | 用沙箱 CA 的哈希目录验证叶证书 → 删除 CA 哈希链接 | trust/<subject-hash>.0 的成功访问消失或变为 ENOENT；不将证书内容变化作为可观察事实 | 若具体 CA 查询路径/结果出现，可定位信任目录；可能仅 stat 查询而遗漏 |
| python-shadow | 使用标准库 json 序列化 → 当前目录新增同名 json.py，遮蔽标准库 | 新的本地 json.py 成功打开，原标准库 json/__init__.py 不再成功打开 | 若展示本地模块路径，可定位模块遮蔽；不应只计退出码 |
| tar-permission | 解包真实项目 README 的构建资料归档 → 输出目录去掉写权限 | 同一个解包目标 README.md 的 openat 从 success 变为 EACCES | 可定位不能写的具体输出；无法单凭 EACCES 区分权限位、ACL、SELinux |
| git-alternates | --shared 克隆从供体读取提交文件 → 移走供体对象目录 | donor/.git/objects/ 下对象访问从 success 变为 ENOENT，或成功访问消失；保留 info/alternates 的关系证据 | 若具体供体对象路径出现，可定位缺失对象存储；仅本地 objects 的常规 ENOENT 不算 |
| checksum-content | sha256sum --check 验证真实源码归档 → 改动归档字节但保持路径 | 内容漂移预计不可直接观察；同路径 open 成功不能证明内容相同。仅退出码变化不算关键线索 | 预计不能从 RunDelta 单独定位内容变化；工具 stderr 可证实校验失败，但不充当 RunDelta 命中 |

## 指标和固定判定

每例先记录 good，再记录同条件 repeat，之后才施加 bad。命令参数、CWD 保持不变；不选择已知故障环境变量注入答案。所有 CLI stdout/stderr、文本/JSON 报告、metadata/events/raw 留存。

三个主要指标：

1. 关键线索是否位于完整 JSON changes 的前 5 条（与文本前五相同），退出/可靠性提示占名次但不算诊断线索。
2. 首个关键线索的 1-based 排名；未出现记 null。checksum-content 只有明确内容证据才算命中，不把同路径成功的新增/消失探测算作内容变化。
3. 能否据此定位问题：分析者依据报告给出“可定位具体检查对象 / 部分线索 / 不能”，并写出用户能采取的下一步及无法推断的部分。没有独立用户参与时明确标为分析者判断，不能伪称用户实测。

无命中、噪声、遗漏和不完整采集均保留。失败试验不更换为更容易的案例，不降低断言或调整排序；若前置环境不满足，记未执行。正常重复差异作为辅助噪声指标。记录源码 SHA-256 与计划 SHA-256 到实验 manifest，结束再次验证源码未改。
