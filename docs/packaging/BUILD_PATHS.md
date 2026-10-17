# release 构建路径归因与处理设计

日期：2026-09-20。本轮没有更改 RunDelta 的 Cargo profile、RUSTFLAGS、构建配置、panic 策略或错误输出。release 二进制仍需后续处理；安全的源码 crate 不等于二进制已经脱敏。

## 实际归因

在不含原工作区产物的独立源码副本，以 Rust/Cargo 1.98.1、Linux x86_64 执行默认 `cargo build --release`。`readelf -W -S` 中没有 `.debug_*` 节。按文件偏移定位的 **52 个私人构建根下 `.rs` 路径全部属于 `.rodata`**：

| 来源 | 路径字符串数 | 脱敏示例 |
|---|---:|---|
| 本次 Cargo registry 源码缓存 | 32 | `${CARGO_HOME}/registry/src/<registry>/clap_builder-4.6.7/src/parser/arg_matcher.rs` |
| 当前 Rust sysroot 的标准库源码 | 20 | `${SYSROOT}/lib/rustlib/src/rust/library/alloc/src/collections/btree/node.rs` |

通过 `readelf -W -r` 的 `R_X86_64_RELATIVE` 引用，读取文件指针、长度、行和列，在本机当前 `core::panic::Location` 布局下找到 **228 个符合该布局且源文件/行列有效的记录**，覆盖全部 52 个路径字符串。抽查对应行是断言、unwrap、索引等调用位置。这个结论来自 ELF 与源码坐标核对，不是仅凭字符串外观判断；没有主动触发全部 228 个潜在 panic，也不把该布局当作其他 Rust/目标平台的稳定 ABI。

| 类别 | 本轮证据与边界 |
|---|---|
| 调试信息 | 当前 release 没有 DWARF 调试节；52 个路径不是需要加载调试文件才出现的信息 |
| panic / caller 位置 | `.rodata` 路径及上述 Location 记录是强证据，位置来自编译的依赖及标准库代码；并非目标程序的用户数据 |
| 源码常量 | 产品有公开的临时目录拼写规则，单元代码也有合成路径；它们不是本机用户名或本次构建根，未更改。不能为了消除任意斜杠字符串而破坏排序/诊断 |
| 嵌入资源 | 本产品唯一 include_str fixture 在 cfg(test) 下，release 不编入该文件；未发现本机记录作为 release 资源被嵌入。未对全部依赖做任意资源的形式化排除证明 |
| 运行期路径 | 由被测命令参数、syscall 和记录位置产生，具有诊断意义；不能在产品报告中隐藏或替换它们来处理构建路径 |

sysroot 路径可以来自标准库内联/泛型位置常量。不能仅凭其前缀断言它一定来自不可重映射的预编译机器码，或断言一定要重建标准库。RunDelta 加重映射后的最终产物是否消除全部这类路径，**本轮未测试**。

## 两组独立实验

复制二进制后分别执行 `strip --strip-debug` 与 `strip --strip-all`；仅修改私人临时副本，不修改原产物。两份副本都仍包含 **52/52 同样的路径字符串**。因此 strip 不是本例的隐私解决方案；strip-all 还减少调试可用的符号。

另建一个无依赖 Rust 小探针，含 file!、panic、普通路径字面量、include_str 内容和运行期参数。相同源码以 `-C opt-level=3 -C debuginfo=0` 编译，另一份只对探针添加 `--remap-path-prefix`：

| 检查 | baseline | remapped |
|---|---|---|
| file! 与 panic 来源 | `${PROBE}/probe.rs` | `/public/probe/probe.rs` |
| panic 错误消息 | 保留 | 保留 |
| 退出码 | 101 | 101 |
| 普通字面量 | 原值 | 原值 |
| include_str 的资源内容 | 原值 | 原值 |
| 运行期 argv 路径 | 原值 | 原值 |

该探针证明编译器位置映射与运行期用户路径是不同层次，不证明已解决 RunDelta 的全部嵌入路径。[rustc 路径重映射说明](https://doc.rust-lang.org/rustc/remap-source-paths.html) 也将其界定为编译器生成路径的尽力处理，而非通用内容脱敏。

## 最小候选设计（未实施）

1. 后续构建实验固定 Rust/目标、Cargo.lock、链接器和缓存输入，分别将源码根、Cargo registry 根、sysroot 来源根映射到不同且稳定的公开前缀。先枚举真实来源，再定映射；不要使用隐藏所有字符串的后处理。
2. 在独立 target 中完整重编依赖，扫描最终二进制，而非只检查编译命令。若 sysroot 来源仍在，再定位是否来自预编译对象或外部工具；不预先承诺一个 RUSTFLAGS 就能全清除。本轮未加入 `.cargo/config.toml` 或任何 profile/构建选项。
3. 保留 panic 消息、行列和可映射源码标识；保留 record/diff 的用户输入路径及全部错误。用原有 CLI/可靠性回归和受控 panic 探针核对，不以删除诊断获得扫描通过。
4. 映射有助于降低构建根造成的二进制差异，但不是可重现构建证明。必须在两个不同构建根重建后比较完整产物；日期、工具链、链接器及其他非确定输入仍需独立控制。
5. 调试器和符号服务将看到公开逻辑路径，需要记录“逻辑前缀→对应源码版本”的映射，并在开发调试时配置源码替换。不能为了发布隐私让维护者丢失定位源码的能力。

最小实施应从外置、可复核的构建命令实验开始，经授权再决定是否固化到发布配置。当前推荐继续以源码 alpha 为准备目标，不直接分发本轮默认二进制。

## 复现检查

`BIN` 指向临时构建的默认 release。以下只读检查；strip 只能对额外副本运行：

```bash
readelf -W -S "$BIN"
readelf -W -r "$BIN"
strings -a -t x "$BIN"
rustc --print sysroot
```

独立探针复现，`PROBE` 必须是尚不存在的临时目录，且不属于仓库：

```bash
python3 - "$PROBE" <<'PY'
import json, subprocess, sys
from pathlib import Path
p = Path(sys.argv[1]).resolve()
p.mkdir()
(p/'resource.txt').write_text(str(p/'embedded-resource'))
source = 'const LITERAL: &str = '+json.dumps(str(p/'literal-value'))+';\n'
source += '''fn main() {
    println!("source={}", file!());
    println!("constant={}", LITERAL);
    println!("resource={}", include_str!("resource.txt"));
    println!("runtime={}", std::env::args().nth(1).unwrap());
    panic!("path probe still reports failure");
}'''
(p/'probe.rs').write_text(source)
for name, flags in [('baseline', []), ('remapped', ['--remap-path-prefix='+str(p)+'=/public/probe'])]:
    subprocess.run(['rustc', '--edition=2024', '-C', 'opt-level=3', '-C', 'debuginfo=0',
                    str(p/'probe.rs'), '-o', str(p/name), *flags], check=True)
    result = subprocess.run([str(p/name), str(p/'runtime-value')], capture_output=True, text=True)
    assert result.returncode == 101
    print(name, result.stdout, result.stderr)
PY
```

探针输出含本次私人工作根，仅用于本地验证，不应原样提交。本文没有保存原始 strings、ELF 转储、探针源码副本、二进制或私人根路径。
