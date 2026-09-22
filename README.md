# RunDelta

![RunDelta signal dashboard](assets/rundelta-signal.svg)

> **See what changed between runs.**
> **看清两次运行之间发生了什么变化。**

`v0.2.0-alpha.1` · Linux · Rust · MIT

[English](#english) · [中文](#中文) · [Project / 项目说明](docs/PROJECT.md) · [Build / 构建](docs/BUILDING.md) · [Architecture / 架构](docs/ARCHITECTURE.md) · [Changelog / 变更](docs/CHANGELOG.md)

---

## 中文

RunDelta 是实验性的 Linux 命令运行差异观测工具。它调用外部 `strace`，保存受支持的执行与文件访问观察，再比较两次真实运行，并保留每项观察的原始证据位置。

它帮助回答“同一条命令这次与上次有什么不同”，不是通用根因判断器。实际执行程序、文件访问结果或启动环境的变化是排查线索，不是因果证明；同一路径不证明同一文件或相同内容。

### 工作流

```bash
rundelta record good -- python build.py
rundelta record bad -- python build.py
rundelta diff good bad
rundelta diff good bad --json
rundelta list
rundelta show good
```

尽量保持命令、参数和初始工作目录一致，每次只改变一个可解释条件。参数独立传递，不隐式进行 shell 展开。使用可重复的 `--env NAME` 显式选择保存的启动环境变量；这不代表后续全部子进程环境。

### 此次 alpha 重构

- 使用 Linux pidfd/signalfd 监督采集器与后端版本探测，区分目标结果、采集器结果、取消和证据完整性。
- 采集事实冻结后，最终化期间迟到的控制信号保留调用方原有处理语义；记录 writer 一次性完成，未发布临时文件在错误路径清理。
- 逐行解析 trace，增量写入及校验 events；晚发 UTF-8 或 I/O 错误不会把已读前缀提升为完整证据。这不是全流程恒定内存承诺。
- 显式提供软时长、原始 trace 大小与终止宽限设置；默认不限制时长和大小。
- 使用独立 `FsContext` 表达已证明的文件系统上下文共享、复制和分离；证据不足时保留不确定性，而不是拼接路径或猜测关系。

```bash
rundelta record bounded \
  --max-duration-ms 30000 \
  --max-trace-bytes 67108864 \
  --terminate-grace-ms 2000 \
  -- /usr/bin/true
```

大小上限是轮询的软边界，可能超出；时长从正式采集器启动尝试前计时，不含版本探测。达到上限后先 TERM，等待宽限（默认 2 秒），必要时 KILL。限额终止的记录是不完整证据，通常返回 125；取消是尽力而为，不能回滚目标已产生的副作用。

### 可靠性与退出码

`diff` 的文本和 JSON 共用同一结论：

| 退出码 | 含义 |
| ---: | --- |
| 0 | 两次采集对受支持事件完整、存储已验证，未观察到差异 |
| 1 | 两次采集对受支持事件完整、存储已验证，观察到差异 |
| 2 | 工具、加载、损坏或不支持错误；不提供普通比较结论 |
| 3 | 采集不完整或未知，或存储未经验证/未最终完成；观察仍可浏览 |

`record` 通常保留目标退出码；目标信号映射为 `128 + signal`，采集失败为 125，初始化或存储错误为 2。这些数字可能与目标退出码重叠，须结合元数据判断。

新记录保持 metadata v3、events v2、finalization v1，diff JSON 保持 v4。历史 metadata v1/v2 和 events v1 可读；未经验证的历史记录不会自动升级为 verified。最终标记只验证结构化 metadata/events，不证明采集成功、不覆盖原始 trace、不认证来源，也不保证断电持久性。

### 安装与运行要求

当前 alpha 仅面向 Linux，需要 Rust/Cargo、Linux pidfd 支持、外部兼容 `strace` 以及追踪子进程的权限。未建立最低 Rust/strace 版本或跨架构、跨发行版支持矩阵。请先按[构建说明](docs/BUILDING.md)选择仓库外的构建目录，再执行：

```bash
cargo build --release --locked
cargo install --path . --locked
rundelta --help
```

RunDelta 不捆绑 strace，也绝不在 tracer 不可用时直接执行目标。可用 `RUNDELTA_STRACE` 指定可信可执行文件。监督启动前要求 `SIGCHLD` 为默认处理且无 `SA_NOCLDWAIT`；忽略信号、自定义处理器或自动回收环境会被拒绝。特权或守护化 tracer wrapper 不在支持边界内。

### 数据与限制

记录可能包含参数、路径、原始证据和选定环境变量；内容不会自动脱敏，分享前须自行审查。默认目录为 `$XDG_DATA_HOME/rundelta`，否则为 `$HOME/.local/share/rundelta`，可用 `--storage` 覆盖。

```bash
rundelta delete good
cargo uninstall RunDelta
```

`delete` 永久删除记录及其证据；卸载只移除程序，不删除记录。追踪会影响时序；不提供自动修复、文件内容差异、完整重放、网络内容采集或未记录运行的事后恢复。CLI 和 JSON 字段保持英文。更多生命周期、路径与存储限制见[项目说明](docs/PROJECT.md)。

---

## English

RunDelta is an experimental Linux command-execution observation tool. It uses an external `strace` to save supported execution and file-access observations, then compares two real runs while preserving raw evidence locations.

It asks what changed between runs, not what universally caused a failure. Changes in executed programs, file-access outcomes, and selected startup environment values are investigation clues, not causal proof. Identical paths do not prove file identity or content equality.

### Workflow

```bash
rundelta record good -- python build.py
rundelta record bad -- python build.py
rundelta diff good bad
rundelta diff good bad --json
rundelta list
rundelta show good
```

Keep the command, argv, and initial working directory fixed where possible, changing one explainable condition at a time. Arguments are passed separately without implicit shell expansion. Repeatable `--env NAME` explicitly selects startup environment values to record; it does not describe every later child environment.

### This alpha refactor

- Linux pidfd/signalfd supervision covers the collector and backend version probe, keeping target outcome, collector outcome, cancellation, and evidence completeness separate.
- After capture facts freeze, late control signals during finalization retain the caller's original dispositions. The record writer finalizes once and cleans up unpublished temporary files on error.
- Trace parsing is incremental, as are event writing and verification. Late UTF-8 or I/O failures cannot promote an already-read prefix to complete evidence. This is not a constant-memory guarantee for the entire workflow.
- Explicit soft duration, raw-trace size, and termination-grace options are available. Duration and size are unlimited by default.
- Per-`FsContext` reasoning models proven sharing, copying, and separation. Insufficient evidence remains uncertain instead of being replaced by guessed relationships or reconstructed paths.

```bash
rundelta record bounded \
  --max-duration-ms 30000 \
  --max-trace-bytes 67108864 \
  --terminate-grace-ms 2000 \
  -- /usr/bin/true
```

The trace-size boundary is sampled and may overshoot. Duration starts immediately before the formal collector spawn attempt, excluding the version probe. A limit sends TERM, waits the grace period (2 seconds by default), then uses KILL if needed. A limited capture is incomplete and normally returns 125. Cancellation is best effort and cannot undo side effects already produced by the target.

### Reliability and exit codes

Text and JSON share the same `diff` decision:

| Code | Meaning |
| ---: | --- |
| 0 | Both captures complete for supported events, storage verified, no observed differences |
| 1 | Both captures complete for supported events, storage verified, observed differences |
| 2 | Tool, load, corruption, or unsupported-data error; no ordinary comparison conclusion |
| 3 | Capture incomplete/unknown or storage unverified/unfinalized; observations remain browsable |

`record` normally preserves the target exit code. A target signal maps to `128 + signal`; capture failure returns 125; initialization and storage errors return 2. These numbers may overlap with target exit codes, so inspect metadata and diagnostics.

New records retain metadata v3, events v2, and finalization v1; diff JSON remains v4. Historical metadata v1/v2 and events v1 remain readable. Unverified historical evidence is not automatically promoted to verified. A finalization marker verifies only structured metadata/events: it does not prove capture success, cover raw traces, authenticate origin, or guarantee power-loss durability.

### Installation and runtime requirements

This alpha targets Linux and requires Rust/Cargo, Linux pidfd support, a compatible external `strace`, and permission to trace child processes. No minimum Rust/strace version or cross-architecture/distribution support matrix has been established. First choose an external build directory as described in the [build guide](docs/BUILDING.md), then run:

```bash
cargo build --release --locked
cargo install --path . --locked
rundelta --help
```

RunDelta does not bundle strace or fall back to untraced target execution. Set `RUNDELTA_STRACE` to a trusted executable when necessary. Before a supervised spawn, `SIGCHLD` must have its default disposition without `SA_NOCLDWAIT`; ignored SIGCHLD, custom handlers, and automatic-reaping environments are rejected. Privileged or daemonizing tracer wrappers are outside the supported boundary.

### Data and limits

Records may contain argv, paths, raw evidence, and selected environment values. They are not automatically redacted; review them before sharing. The default data root is `$XDG_DATA_HOME/rundelta`, otherwise `$HOME/.local/share/rundelta`; use `--storage` to override it.

```bash
rundelta delete good
cargo uninstall RunDelta
```

`delete` permanently removes the record and its evidence. Uninstall removes the executable, not recorded data. Tracing affects timing. RunDelta offers no automatic repair, file-content diffing, complete replay, network-content capture, or retrospective recovery of unrecorded runs. CLI and JSON fields remain English. See the [project guide](docs/PROJECT.md) for lifecycle, path, and storage boundaries.

## License / 许可证

RunDelta is licensed under the MIT License; see [LICENSE](LICENSE).
RunDelta 使用 MIT 许可证，详见 [LICENSE](LICENSE)。
Copyright (c) 2026 The RunDelta contributors.
