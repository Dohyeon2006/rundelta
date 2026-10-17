# A3 调用链、复杂度模型与候选设计

基线：`d12106d72fab1ff480087a3580f47ac761a6953d`。本文件只描述候选方案，不实现或运行替代完成检查。

## 只读调用链审查

`main::run → capture::record` 启动并等待后端后：

| 阶段 / 位置 | 范围与次数 | 结构推断（不是单独实测） |
|---|---|---|
| capture.rs:156–157 → parse_with_cwd | 读整个 trace；逐行解析一次 | 按原始字节/行数增长 |
| parse::resolve_context，parse.rs:335 起 | 生成全部事件索引并按证据起始行排序；全量 any 判断 shared；按索引访问所有事件，维护 PID→CWD | 排序最坏 O(E log E)，上下文 map 约 O(E log P)，字符串成本另计 |
| capture.rs:165–169，root_pid | 从 events 开头找第一个成功 exec，最多 E 次谓词访问；只调用一次 | O(E) 上界，本输入第一次就命中 |
| capture.rs:170–176，root_exit | 对该 root PID 从末尾反向找最后 exit，最多 E 次；只调用一次 | O(E) 上界，本输入根 exit 在最后 |
| capture.rs:198–208，missing_exits | 对每个事件的 pid 及 Some(child_pid) 生成候选；每候选从 events 开头 any 搜索 exit，最后才把缺失 PID 收入 BTreeSet | Q 次搜索，每次最多 E；主要上界 O(QE)，缺失集合维护另有 O(Q log P) |
| capture.rs:209–255 | 对缺失 PID 生成警告，解析目标与采集器状态、处理中断/未知信号 | 不能因性能改造而跳过；本次计时区间不含警告拼接 |
| capture.rs:257 → store::finish | 遍历全部 events 编码；写入同步/rename；全量读回、字节相等检查、反序列化计数；metadata 检查及最终 marker | 按事件/字节数增长及 I/O 等待，不含 missing_exits 的 PID 二重扫描 |
| main show → store::load → resolve/scan → read_run | 目录 metadata 扫描；所选记录 marker/metadata/events 读入、摘要/字节数/事件数验证和反序列化 | **不调用 capture 的 missing_exits**；不能把 show 的时间说成完成检查时间 |
| main show 输出 | 序列化完整 Run，写 stdout | 端到端 show 测量包括这一项，本轮未单独分离 |

E 为标准化事件数，P 为唯一 PID 数，S 为非空 child_pid 字段数，Q=E+S。原代码在每一个出现处 clone PID，即使同一 PID 已出现很多次也会重搜。BTreeSet 只去重最终**缺少退出**的结果，不去重搜索输入；全正常、缺失集合为空仍会有重复搜索。

设 PID p 的出现次数（含 child_pid）为 c(p)，它在 events 中第一个 kind=exit 的位置（1 起始）为 j(p)，没有 exit 时 j(p)=E。代码模型预测的 any 谓词访问数为：

`V = Σ c(p) × j(p)`。

这是依据代码与合成事件排列计算的计数，不是硬件计数器实测。root_exit 取该 PID 的**最后** exit，而 missing_exits 只问是否存在任意 exit，候选方案必须同时保留这一区别。

主矩阵所有 exit 位于尾部。P=1 时 S=0，c=E，j=E，因此精确预测 V=E²：4,000,000 / 64,000,000 / 1,024,000,000。一般 Q 与 E 同阶，最坏为平方级事件扫描。固定 E 增加 P 并不自动乘出一个额外 P 因子；本输入 Q=E+P-1，exit 位置也随 P 改变。固定每进程事件量而增加 P 会同时增大 E，不可据此声称独立的进程数效应。

## 单次遍历候选（未实现）

候选是具体的 per-PID 聚合，不引入采集后端 trait、插件或共享所有权框架。

每个 PID 保留：

- 是否在 event.pid 或任意非空 child_pid 中出现；只有 child_pid 出现而没有自身事件的进程也需要检查。
- 是否见到 kind=exit（与原谓词一致，不擅自要求信号已识别或退出码可解析）。
- 最后一个 exit 的事件索引，用于根进程退出原始信息与 evidence 回溯。

全局保留第一个 `kind=exec && outcome=success` 的 PID。按原 events Vec 顺序遍历一次，更新有序 PID 表。最后按表的原字符串字典序输出未见 exit 的 PID，保持现有 BTreeSet 的去重和排序；根 PID 对应最后 exit 索引用于现有状态判定。

不能简单只跟踪“根 exec 之后的 exit”：旧算法从整个 Vec 反向找，异常/不完整输入中可能存在更早的 exit。为保持行为，按 PID 保存最后 exit，再确定根的那一条。不要按 evidence 时间重新排序整个事件流，不要把 PID 复用问题当作本性能修复顺便解决。

以 BTreeMap 借用 PID 字符串为一个可审查的方向：O(E log P + P) 时间、O(P) 状态；结束时只为警告分配必要字符串。是否改用 hash 集合应基于额外测量，并单独保留警告排序；本轮没有比较数据结构实现。没有实现候选、没有测得候选耗时或加速倍数。

## 语义保持与校验边界

- 根目标退出码、终止信号、原始未知信号、采集器退出状态、记录器中断仍由原状态逻辑表达；“存在 exit”不等于“确认采集完整”。
- 无根 exec/无根 exit、部分子进程缺 exit、未知信号、解析警告、CWD 相对不确定、shared FS/namespace 警告继续导致原有的不完整语义；不能为了让 benchmark 变快或返回 0 而删掉警告。
- 集合聚合只替换重复搜索，不改事件内容、证据、pid/child_pid，不改 capture 等待/信号/锁/生命周期，不提前 finalized。
- events 完整序列化、读回验证、摘要、事件数、metadata 和最后 marker 顺序全部保留。不能把计算过退出集合当作跳过这些存储校验的理由。
- v1/v2/v3 记录读取当前不经过此 capture 检查，因此理论上无需格式迁移或重新解析。新采集的持久化字段与旧版本一致；仍须跑 legacy 及损坏回归，不因理论上不受影响而省略验证。
- 当前 PID 复用、多次 exit、同一路径含义等模型边界保留，不借性能任务扩大正确性承诺。

## 进入实现前应增加的回归

1. 大量同 PID 事件、exit 在最前/中间/末尾/缺失；验证准确警告，而非只比较耗时。
2. 大量进程、仅 child_pid 出现、重复 child_pid、失败 spawn（无 child_pid）、父子交错、不同 PID 字符串长度及字典序，验证缺失列表严格一致。
3. 多次成功 exec、失败 exec 在前、同 PID 多条 exit，验证根选择和最后 exit 保持，原始 unknown signal 不丢失。
4. 空/截断事件、无根、未知信号、目标 return 128+signal 与真实信号、采集器故障、中断、部分子进程无结束，验证目标/采集器/采集完整性仍分开。
5. CWD 不确定和 shared FS 案例：优化前后所有事件与警告不变，diff 3 不被优化成 0/1。
6. 用新的独立 fixture 做旧/新判定结果对照，保留现有冻结回归输入和断言，不用它们调算法。
7. 原完整行/半行/缺文件/内容修改/缺 marker、旧 v1/v2 unverified 与 v3 verified、不安全索引和并发锁回归全部运行。
8. 相同 E×P 矩阵重新测量，先校验结果完全一致再比较增长趋势；不要将易受负载影响的绝对毫秒上限当作功能测试。

预计收益仅为移除已确认的大量重复谓词访问；实际端到端收益受解析、分配、序列化、读回与 fsync 限制，不能从比较次数比例直接转换为实际加速倍数。
