# Building and checking / 构建与检查

[README](../README.md) · [Project / 项目说明](PROJECT.md)

## English

### Requirements and isolated build output

The source targets Linux and requires Rust/Cargo. Real recording additionally needs Linux pidfd support, a compatible trusted external strace, and permission to trace children. No minimum Rust/strace version or cross-distribution/architecture support matrix is established. Python 3.11 or newer runs the public CLI integration tests, independent export checker, and checker tests; optional real-capture acceptance also needs `cc`.

Before building, set `RUNDELTA_BUILD_DIR` to a user-chosen **absolute directory outside this checkout**. The checker rejects all unexpected source-tree files, including ignored build output; a Git ignore rule is not an export allowlist.

```bash
export CARGO_TARGET_DIR="${RUNDELTA_BUILD_DIR:?set an external absolute build directory}"
cargo build --release --locked
cargo test --locked
if [ -e target ]; then rmdir target; fi
cargo clippy --all-targets -- -D warnings
cargo fmt --check
python3 -B tests/test_export_safety.py
```

Use a fresh external build directory when validating a new candidate. Do not copy existing binaries or recordings into the source tree. Frozen unit tests leave an empty scratch `target` parent even with external Cargo output; `rmdir` removes only that empty directory and fails on any leftover content. Do not recursively clean away scan inputs. `cargo test --locked` includes synthetic CLI integration on Linux with Python, without needing strace or `cc`. `cargo install --path . --locked` installs the `rundelta` executable; the package name is `RunDelta`.

### Linux capture acceptance (required for release candidates)

```bash
cargo test --locked --test public_cli -- --ignored
```

Set `RUNDELTA_STRACE` to a trusted executable if strace is not in PATH. The public acceptance creates its own synthetic inputs and fresh records; it does not need saved runs, external issue evidence, or untracked fixtures. These tests exercise real Linux process/trace behavior and require the necessary local permissions. Ordinary unit tests and package tests are not a claim that every platform or lifecycle has been validated.

The same entry point also builds the packaged finalization barrier and checks late HUP/INT/TERM with default, ignored, and blocked dispositions on success and injected marker failure. These cases use a finite synthetic backend, not private probes. Missing prerequisites, skipped cases, and expected failures are errors in the CLI runner.

### Export and package closure

In the Git checkout, inspect the entire working tree and reachable candidate history, then build and scan the source package:

```bash
python3 -B tools/check_export.py --root . --history
cargo package --list --locked
cargo package --locked
python3 -B tools/check_export.py --root . --history \
  --archive "$CARGO_TARGET_DIR/package/RunDelta-0.2.0-alpha.1.crate"
```

Use a clean committed candidate for the package commands; do not pass `--allow-dirty`. The explicit Cargo include list and the checker allowlists must agree. The crate contains its public tests, required fixtures, checker, and checker tests; no private or untracked fixture may complete the package.

Set `RUNDELTA_UNPACK_DIR` to a new user-chosen external directory. Test the actual package, not the original checkout:

```bash
mkdir -p "${RUNDELTA_UNPACK_DIR:?set a new external unpack directory}"
tar -xf "$CARGO_TARGET_DIR/package/RunDelta-0.2.0-alpha.1.crate" \
  -C "$RUNDELTA_UNPACK_DIR"
(
  cd "$RUNDELTA_UNPACK_DIR/RunDelta-0.2.0-alpha.1"
  export CARGO_TARGET_DIR="$RUNDELTA_UNPACK_DIR/build"
  cargo test --locked
  if [ -e target ]; then rmdir target; fi
  cargo test --locked --test public_cli -- --ignored --exact public_cli_workflow
  python3 -B tests/test_export_safety.py
)
```

The independent safety scanner checks allowed files, fixture closure, manifest packaging policy, and content in plain text, JSON strings, escaped strings, hexadecimal, and common standard/URL-safe base64 forms. History inspection includes older reachable commits, not just the tip. Negative tests cover unexpected files, missing fixtures, incorrect Cargo includes, dirty archives, and historical leaks. A scan cannot prove the absence of unknown secrets or arbitrary encodings; new files and exceptions require review.

### Public CI and local equivalent

The workflow in `.github/workflows/ci.yml` runs these commands on Ubuntu 24.04 with stable Rust, Python, `cc`, and strace. It checks pull requests, pushes to main/public-export, version-tag pushes, and manual runs. Every candidate gate above, including real CLI and unpacked-package checks, is required; nothing publishes a package, tag, binary, or recording.

Checkout is pinned by commit, fetches complete history, and does not persist credentials. Pull requests validate their actual head with read-only permissions, not a synthetic merge commit. The export policy requires the reviewed public root and a linear candidate ancestry: retain that history with a fast-forward integration. The workflow itself is source-only and deliberately excluded from the crate; its fixtures and checker are included.

For a local equivalent, use a clean isolated checkout, install the stated prerequisites, run all commands above in order, then confirm `git status --porcelain=v1 --untracked-files=all` is empty and repeat the history scan. This checks the workload locally; only a later GitHub run can validate runner provisioning and hosted execution.

Also validate GitHub-specific workflow expressions before committing. Install [actionlint](https://github.com/rhysd/actionlint) outside the checkout (CI pins version 1.7.12 and verifies its archive SHA-256), then run:

```bash
python3 -B tests/test_export_safety.py --workflow actionlint
```

This source-only mode checks the actual workflow and rejects a synthetic `runner.temp` expression at job-level `env`, while accepting it at step scope. The workflow sets build/unpack paths from `$RUNNER_TEMP` through `$GITHUB_ENV` during a step. Plain YAML parsing or replaying shell commands does not validate GitHub context availability. Ordinary export tests and unpacked-crate tests do not need actionlint; the workflow itself remains excluded from the crate. No production dependency is added.

### Binary distribution is a separate review

Rust binaries may retain compiler source locations, dependency-cache locations, and embedded data. Stripping symbols does not necessarily remove paths. An external release build can use rustc's `--remap-path-prefix` with reviewed logical prefixes and a fresh build directory, but source-package safety does not certify a binary. Scan any proposed binary separately, review dependency licenses, and retain useful diagnostics. This source export includes no prebuilt executable and makes no reproducible-binary claim.

## 中文

### 要求与隔离构建输出

源码面向 Linux，需要 Rust/Cargo。真实录制还要求 Linux pidfd、兼容且可信的外部 strace，以及追踪子进程的权限。尚未建立最低 Rust/strace 版本或跨发行版/架构矩阵。公开 CLI 集成测试、独立导出检查器及其测试需要 Python 3.11 或更新版本；可选真实采集验收还需要 `cc`。

构建前，把 `RUNDELTA_BUILD_DIR` 设为用户选择的**仓库外绝对目录**。检查器拒绝源码树中所有未允许文件，包括已忽略的构建产物；Git ignore 不等于导出白名单。

```bash
export CARGO_TARGET_DIR="${RUNDELTA_BUILD_DIR:?set an external absolute build directory}"
cargo build --release --locked
cargo test --locked
if [ -e target ]; then rmdir target; fi
cargo clippy --all-targets -- -D warnings
cargo fmt --check
python3 -B tests/test_export_safety.py
```

每次验证新候选应使用新的外部构建目录，不把已有二进制或录制数据复制到源码树。冻结单测即使使用外部 Cargo 输出也会留下空 `target` 父目录；`rmdir` 仅删除空目录，存在残留内容时会失败，不得递归清理后再扫描。`cargo test --locked` 包含 Linux/Python 合成 CLI 集成测试，不需要 strace 或 `cc`。`cargo install --path . --locked` 安装 `rundelta` 程序；Cargo 包名是 `RunDelta`。

### Linux 采集验收（发布候选必跑）

```bash
cargo test --locked --test public_cli -- --ignored
```

若 strace 不在 PATH，设置 `RUNDELTA_STRACE` 指向可信可执行文件。公开验收创建合成输入与新鲜记录，不依赖历史记录、外部 issue 证据或未跟踪 fixture。真实进程与追踪测试需要本机权限；普通单元测试或包测试通过，不表示所有平台或生命周期均已验证。

同一入口还编译包内最终化 barrier，验证迟到 HUP/INT/TERM 在默认、忽略、阻塞处理方式下的成功与 marker 失败路径。它使用有限合成后端，不依赖私有 probe。CLI runner 将缺失前置条件、跳过及预期失败都视为失败。

### 导出与包闭包

在 Git checkout 中检查整个工作树和候选可达历史，然后打包与扫描：

```bash
python3 -B tools/check_export.py --root . --history
cargo package --list --locked
cargo package --locked
python3 -B tools/check_export.py --root . --history \
  --archive "$CARGO_TARGET_DIR/package/RunDelta-0.2.0-alpha.1.crate"
```

打包时使用干净且已提交的候选，不使用 `--allow-dirty`。Cargo 显式 include 与检查器白名单须一致；crate 包含公开测试、所需 fixture、检查器及其测试，不能借助私有或未跟踪文件补齐。

把 `RUNDELTA_UNPACK_DIR` 设为用户选择的新外部目录，然后测试真正的解包结果：

```bash
mkdir -p "${RUNDELTA_UNPACK_DIR:?set a new external unpack directory}"
tar -xf "$CARGO_TARGET_DIR/package/RunDelta-0.2.0-alpha.1.crate" \
  -C "$RUNDELTA_UNPACK_DIR"
(
  cd "$RUNDELTA_UNPACK_DIR/RunDelta-0.2.0-alpha.1"
  export CARGO_TARGET_DIR="$RUNDELTA_UNPACK_DIR/build"
  cargo test --locked
  if [ -e target ]; then rmdir target; fi
  cargo test --locked --test public_cli -- --ignored --exact public_cli_workflow
  python3 -B tests/test_export_safety.py
)
```

独立扫描覆盖文件白名单、fixture 闭包、Cargo 打包策略，以及普通文本、JSON 字符串、转义、hex 和常见标准/URL-safe base64。历史检查覆盖较早可达提交，不只检查最新版本。负向测试覆盖未允许文件、缺失 fixture、错误 Cargo include、脏归档及历史泄露。扫描不能证明所有未知凭据或任意编码均不存在；新文件与新例外仍需审查。

### 公开 CI 与本地等价验证

`.github/workflows/ci.yml` 使用 Ubuntu 24.04、stable Rust、Python、`cc` 和 strace。触发条件为 PR、main/public-export 推送、版本 tag 推送及手动运行。上述全部候选检查（含真实 CLI 和解包测试）必须通过；工作流不发布包、tag、二进制或录制数据。

Checkout 固定到提交、获取完整历史且不保留凭据。PR 以只读权限验证真实 head，而非合成 merge 提交。导出策略要求经审查公开根提交及线性候选历史，应通过 fast-forward 集成保留该历史。workflow 只纳入源码仓，不进入 crate；所需 fixture 和检查器进入 crate。

本地等价验证使用干净隔离 checkout，安装上述前置依赖，按序执行全部命令，最后确认 `git status --porcelain=v1 --untracked-files=all` 为空并复跑历史扫描。本地通过只验证工作负载；GitHub runner 配置与托管执行仍需日后实际运行验证。

提交前还须验证 GitHub 专有的工作流表达式。在仓库外安装 [actionlint](https://github.com/rhysd/actionlint)（CI 固定 1.7.12 并验证下载包 SHA-256），然后运行：

```bash
python3 -B tests/test_export_safety.py --workflow actionlint
```

此源码专用模式检查实际 workflow，并用合成反例拒绝 job 级 `env` 中的 `runner.temp`，同时确认该表达式在步骤级可用。工作流在步骤执行时从 `$RUNNER_TEMP` 生成构建/解包路径，通过 `$GITHUB_ENV` 传给后续步骤。普通 YAML 解析或重放 shell 命令不能验证 GitHub 上下文可用性。普通导出测试及解包测试不需要 actionlint；workflow 仍不进入 crate，也不增加生产依赖。

### 二进制分发需要单独审查

Rust 二进制可能保留编译位置、依赖缓存位置及嵌入数据；strip 不一定删除路径。可在外部全新构建目录使用 rustc `--remap-path-prefix` 和经审查的逻辑前缀，但源码包安全不代表二进制安全。拟分发二进制必须独立扫描、检查依赖许可证并保留有用诊断。本导出不含预编译程序，也不承诺可重复二进制构建。
