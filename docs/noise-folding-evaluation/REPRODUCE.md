# 样例、复现与证据管理

## 环境和条件

本次实测 Linux 7.2.5-200.fc44.x86_64、GCC 16.2.1、CMake 3.31.2、Cargo/rustc 1.98.1、Python 3.12、pip 25.1.1、strace 6.16。应以普通用户运行，需允许 ptrace；pip 的 chmod 000 场景不能用 root。需要 Rust 工具链、C/C++ 编译器、make、CMake 和包含 pip 的 Python。测试安装完全离线，使用脚本自行生成的最小 wheel，不下载被测依赖。准备工具可能需要联网，不属于被测命令。

在 RunDelta 仓库根目录执行（以下工具位置替换为本机已安装程序）：

```bash
export RUNDELTA_BIN="$PWD/target/debug/rundelta"
export RUNDELTA_STRACE="$(command -v strace)"
export CMAKE_BIN="$(command -v cmake)"
export PIP_PYTHON="$(command -v python3)"
"$PIP_PYTHON" -m pip --version
python3 -B docs/noise-folding-evaluation/collect.py
```

如当前 Python 没有 pip，可先在仓库外用 `python3 -m venv /chosen/private/tool-env` 建环境并安装 pip 25.1.1，再令 PIP_PYTHON 指向它。已有 strace 6.16 位于项目 `target/tools/strace-6.16/src/strace` 时可直接指定，不更改系统配置。RUNDELTA_BIN 必须是已有可用构建；本评估没有修改产品源码。

collect.py 打印 `ARTIFACTS: <新建目录>`。将该路径赋给 ARTIFACTS。每次运行都会生成新目录和新记录，不覆盖旧记录。目录清理只针对脚本新建样例中的 build、target、installed；异常输入均在 finally 中恢复。每阶段限时 180 秒；超时会停止并保留已有数据，不将超时冒充成功。后续工具版本、内核、环境和临时值不同，差异总数可能不同；预期线索不因此修改。

## 实际被测命令与变化

每例独立目录，启动 CWD/argv 在三阶段不变。产品调用均为：

```bash
"$RUNDELTA_BIN" --storage "$ARTIFACTS/records" record <case>-<phase> -- <command> <args...>
"$RUNDELTA_BIN" --storage "$ARTIFACTS/records" diff <case>-good <case>-repeat --json
"$RUNDELTA_BIN" --storage "$ARTIFACTS/records" diff <case>-good <case>-bad --json
```

占位符由 collect.py 使用独立参数传递，不拼接用户命令。record 的实际 stdout、stderr、返回码和所有执行参数写入私有 commands.json。

| 样例 | 命令 / 输入 | 正常重复重置 | bad 唯一注入 |
|---|---|---|---|
| CMake | `sh build.sh`，内部 `cmake -S . -B build`、`cmake --build build --parallel 2`；main.cpp include answer.h | 删除本例 build | 将 include/answer.h 临时移到不会被 include 搜索的 saved-header |
| Cargo | `cargo build --offline --jobs 1`；main.rs 用 include_str! 读取 build-input，无第三方依赖 | 删除本例 target；Cargo.lock 预先生成 | 将 src/build-input 临时移到 saved-input |
| pip | `python -m pip install --no-index --no-deps --no-compile --target installed fixture_payload-0.1-py3-none-any.whl` | 删除本例 installed | wheel chmod 000，结束恢复 0600 |

命令不是模拟采集；CMake/Cargo 真的执行编译，pip 真的安装脚本生成的合法 wheel。注入是受控工具失败，不声称复现第三方 issue。

本次 collect.py 实际输出（原样数值，省略私有目录首行）：

```text
cmake normal changes 162 clue [] exit 1
cmake abnormal changes 174 clue [2] exit 1
cargo normal changes 76 clue [] exit 3
cargo abnormal changes 255 clue [2] exit 3
pip normal changes 36 clue [] exit 3
pip abnormal changes 27 clue [2] exit 3
```

## 候选视图与回溯

```bash
python3 -B docs/noise-folding-evaluation/view.py "$ARTIFACTS" cmake normal --mode both
python3 -B docs/noise-folding-evaluation/view.py "$ARTIFACTS" cmake normal --details G1
python3 -B docs/noise-folding-evaluation/view.py "$ARTIFACTS" cargo abnormal --mode r1
python3 -B docs/noise-folding-evaluation/view.py "$ARTIFACTS" pip abnormal --mode r2
python3 -B -m unittest discover -s docs/noise-folding-evaluation/ -p test_view.py -v
python3 -B docs/noise-folding-evaluation/audit.py "$ARTIFACTS"
```

view.py 的 JSON 保留产品的 `exit_code` 字段及可靠性；分析脚本自身成功运行退出 0，不冒充产品 diff CLI。group ID 仅在该比较/模式内有效，不能跨比较引用。`--details G1` 在 CMake 正常比较中实际返回 4 个原 change、6 个关联事件；Cargo 正常组返回 4 个 change、4 个事件。无组时不提供不存在的 group ID。

本次单元测试实际结果：`Ran 8 tests ... OK`。audit.py 读取六份完整比较 JSON、九份完整 show JSON及 records 文件，共核对 52 个原始文件的前后摘要；三种模式均审计所有原条目唯一覆盖、详情事件、预登记线索和可靠性。RESULTS.json 即审计标准输出，不手工改数值。

## 公共和私有证据的区别

私有 ARTIFACTS 中保留：

```text
manifest.json                       # 采集前计划、源码、二进制摘要
commands.json                       # 精确 argv/CWD/stdout/stderr，可能含私有路径
results.json                        # 采集结果，含完整线索路径
<case>-normal.json                  # 产品完整原始 JSON，未经脱敏或筛选
<case>-abnormal.json                # 同上
<case>-<phase>.show.json            # 元数据与全部标准化事件
records/runs/<id>/                  # 原始记录、最终标记、事件及私有 raw 追踪
```

这些文件没有被删除或重写。候选 `*.view.json` 和 `*.details.json` 是独立派生文件，不能代替原始 JSON。公开 RESULTS.json 存全部 15 个原始报告/show JSON 的 SHA-256，以及计划和分析脚本摘要；EVIDENCE.json 只保留必要摘录，`${ARTIFACTS}`、`${TOOLING}`、`${PYTHON_TOOLING}`、`${HOME}` 是脱敏占位符，不是原始路径。依据同一私有 manifest/run-id 与 evidence.file/line 可查原始记录；公开摘录本身不能还原已脱敏路径。

不将私有目录、原始日志、虚拟环境或构建产物加入 Git。需要分享全量数据时必须另行审查、产生明确标记的脱敏副本并保留私有原件，本轮没有公开全量私有 JSON。
