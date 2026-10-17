# 复现命令与条件

在 Linux x86_64 上，准备现有 RunDelta、支持 `--kill-on-exit` 的 strace（本次 6.16）、Python 3.14.7/venv、git、ar、GCC/G++/make、rustup 及 Rust 1.98.1。无需 sudo、私有资源或凭据。公开源码及依赖下载需要网络，**目标记录阶段不以网络请求作为失败条件**。

从 RunDelta 根目录运行；脚本仅在新建隔离目录写入，不构建或更改 RunDelta：

```bash
python3 -B docs/external-alpha/reproduce.py \
  --rundelta "$PWD/target/debug/rundelta" \
  --strace "$PWD/target/tools/strace-6.16/src/strace"
```

可加 `--case pybind`、`--case meson` 或 `--case spades`。脚本打印私有工作目录；不要把整个目录复制入仓库。每次运行新建目录，不删除或覆盖历史记录。先复制冻结登记，再执行目标。bootstrap 安装工具时不执行目标诊断。

脚本使用固定 CMake/Python/pybind11/SPAdes 版本及 Meson commit，下载附件及兼容库核对 SHA-256。pybind11、Meson、SPAdes 各有只读源码 checkout。Python/CMake 发布包、虚拟环境、兼容库都在隔离目录；没有系统安装。Meson fixture 的 Cargo 使用 `--locked --offline --target-dir`，Meson 使用单独 build 目录和 `--wrap-mode=nodownload`。Python 禁止写字节码。SPAdes 输出位于单独 runtime 目录。

主要目标命令如下（变量均是复现脚本创建的路径）：

```bash
# pybind11；相同 command/cwd，独立的空构建状态。
CMAKE_PREFIX_PATH="$PACKAGE_CMAKE_DIR" rundelta --storage "$STORE" record good \
  --env CMAKE_PREFIX_PATH -- cmake -S "$CONSUMER" -B "$BUILD"
# 将本次 good 的 BUILD 移到 good-build，创建同名空 BUILD，不删除记录。
env -u CMAKE_PREFIX_PATH rundelta --storage "$STORE" record bad \
  --env CMAKE_PREFIX_PATH -- cmake -S "$CONSUMER" -B "$BUILD"

# Meson；两种不同构建入口，不能视为单变量对照。
rundelta --storage "$STORE" record good -- cargo build --offline --locked \
  --manifest-path "$FIXTURE/Cargo.toml" --target-dir "$CARGO_OUTPUT"
rundelta --storage "$STORE" record bad -- python3 -B "$MESON_SOURCE/meson.py" \
  setup "$MESON_OUTPUT" "$FIXTURE" --wrap-mode=nodownload

# SPAdes；各自在对应不含空格/含空格的 runtime CWD 中执行。
rundelta --storage "$STORE" record good-v3 -- "$PLAIN_INSTALL/bin/spades.py" --test --careful
rundelta --storage "$STORE" record bad-v3 -- "$SPACED_INSTALL/bin/spades.py" --test --careful

rundelta --storage "$STORE" diff good bad --json
# SPAdes 最终比较改用 good-v3 bad-v3。
```

正常目标退出为 0，失败目标预期 pybind11=1、Meson=1、SPAdes=4；diff 首轮分别为 1、3、3。**不要使用 `set -e` 把正常的 diff 1/3 当成采集器崩溃**。脚本保存独立 rc 和 stdout/stderr，不要求所有命令返回零。

SPAdes 首次 Python 3.10.12 在本机缺 `libcrypt.so.1`：两次 127；它们不是正常对照。3.10.19 在共享无空格 CWD 下两次成功。最终使用 3.10.12，加本地解包的 Debian libcrypt1 兼容库，且改变工作/输出路径。复现脚本保留这三个阶段；其他机器已有兼容库时，首轮 127 未必发生，不能强造失败。最终条件仍须实测。

验证公开材料及既有冻结材料（全部只读）：

```bash
python3 -B docs/external-alpha/verify.py
python3 -B tests/check_regression_freeze.py
python3 -B docs/external-cases/pybind11-5626/verify.py
python3 -B docs/performance/a3-completion-check/verify.py
python3 -B docs/performance/a3-completion-check/verify_optimization.py
```

没有运行会重建产品的 Cargo 测试或修改现有记录的审计。本阶段要求保持产品及旧工作区只读。
