# RunDelta project guide / 项目说明

[中文](#中文) · [English](#english) · [README](../README.md)

## 中文

### 项目目标

RunDelta 关注一个很具体的问题：两次看似相同的命令运行，实际条件发生了什么变化？

它不是笔记软件、通用 AI 助手或完整调试器。它在 Linux 上追踪一次命令及其子进程的部分系统调用，将采集结果规范化、保存，并比较两次运行中的可验证差异。

```text
目标命令
   │
   ├── strace 采集受支持事件
   ├── RunDelta 解析与保存证据
   └── diff 聚合观察、可靠性状态与原始位置
```

### 设计原则

1. **证据优先。** 报告中的每项观察应能回溯到记录与原始追踪位置。
2. **不确定就说明。** 未完成采集、存储未经验证、相对路径上下文不可靠时，不能输出假定的确定结论。
3. **原始记录与摘要并存。** 易读文本报告不会替代 JSON 和原始证据。
4. **不改写目标程序。** RunDelta 观察目标，不修改其源码或原始文件。
5. **先比较，再解释。** 它展示差异；根因仍需人或其他工具验证。

### 可靠性模型

`diff` 将“是否观察到差异”与“该结论是否可信”分开。可靠性不足时仍可浏览信息，但退出码为 3，调用方不能将其当作稳定的无差异或有差异结论。

存储完整性由 finalization marker 校验 metadata 和 normalized events 的长度、数量与 SHA-256。该校验不验证 raw strace、不抵御恶意篡改，也不保证断电时的持久性。

### 当前适用场景

- 实际 Python、Node、shell 或工具链路径变化；
- 配置文件选择与访问结果变化；
- 构建或脚本运行期间的文件缺失、权限与 `ENOENT` 线索；
- 子进程与受支持执行事件变化。

动态构建系统会产生大量路径变化，文件内容相同与否不在当前模型内。把它用于真实项目时，应尽量固定命令入口，并一次只改变一个可解释条件。

### 状态

这是探索性 alpha。它已经在受控案例与有限外部案例中验证了配置、解释器和工具链路径漂移的诊断价值；它尚未承诺所有 Linux 工作负载、所有构建系统或通用根因判断。

## English

### Goal

RunDelta focuses on one narrow question: what changed between two apparently similar command executions?

It is not a note-taking system, a general AI assistant, or a full debugger. On Linux, it traces a command and its children through a supported subset of system calls, normalizes and stores the evidence, then compares two runs.

```text
target command
   │
   ├── strace captures supported events
   ├── RunDelta parses and stores evidence
   └── diff aggregates observations, reliability, and evidence locations
```

### Principles

1. **Evidence first.** Every observation should lead back to a record and raw-trace location.
2. **State uncertainty.** Incomplete capture, unverified storage, and uncertain relative-path context must not become confident conclusions.
3. **Keep summaries and evidence.** Readable reports do not replace JSON or raw evidence.
4. **Do not rewrite the target.** RunDelta observes a command without modifying its source or original files.
5. **Compare before explaining.** It reports differences; a human or another tool must still validate root cause.

### Reliability model

`diff` separates whether observations differ from whether the comparison is reliable. When reliability is insufficient, observations remain visible but exit code 3 prevents callers from treating the output as a stable difference/no-difference conclusion.

A finalization marker verifies metadata and normalized-event counts, byte sizes, and SHA-256 digests. It does not verify raw strace evidence, authenticate origin, resist malicious edits, or guarantee power-loss durability.

### Good fits today

- Changes in the Python, Node, shell, or toolchain executable that actually ran;
- Configuration-file selection and access-result changes;
- Missing-file, permission, and `ENOENT` clues during builds or scripts;
- Changes in child processes and supported execution events.

Dynamic build systems can create many normal path changes, and file-content equality is outside the current model. For a useful real-project comparison, keep the entry command fixed and change one explainable condition at a time.

### Status

This is an exploratory alpha. Controlled cases and limited external cases have shown value for configuration, interpreter, and toolchain-path drift. It does not promise support for every Linux workload or build system, nor general root-cause determination.
