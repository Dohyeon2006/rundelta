# RF-600：增量 trace parser 的同源码 before/after 基准

日期：2026-09-21。该基准只比较当前源码中的两种 parse 输入适配方式；未修改生产源码，也未把合成 trace 或临时 harness 留在仓库中。

## 结论

在同一个 release harness、同一批已预热的合成输入上，64 KiB `BufRead::read_until` + `TraceParser::push_line/finish` 与兼容的 `read_to_string` + `parse_with_cwd` 产生了逐事件、逐 warning、raw line count 完全相同的结果。

GNU time 的 wall-time 中位数在 0.01 秒报告精度下没有可辨差异。增量模式的 max RSS 中位数分别下降 **10.17% / 10.63% / 10.76%**；绝对差值随输入从约 4.3 MiB 增至 52.1 MiB，基本对应不再保留的整份 raw `String`。这只证明本 harness 的 parse 输入副本被移除，不能外推为端到端 record 的同等降幅。

| raw 行数 | 输入字节 | events | warnings | before wall 中位数 | after wall 中位数 | before RSS 中位数 (KiB) | after RSS 中位数 (KiB) | RSS 改善 |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 100,000 | 4,550,000 | 75,000 | 1 | 0.04 s | 0.04 s | 43,312 | 38,908 | 10.17% |
| 400,000 | 18,200,000 | 300,000 | 1 | 0.17 s | 0.17 s | 166,736 | 149,020 | 10.63% |
| 1,200,000 | 54,600,000 | 900,000 | 1 | 0.51 s | 0.51 s | 495,608 | 442,272 | 10.76% |

## 被测边界

临时 harness 直接以 `#[path]` 引入本工作树的 `src/model/mod.rs` 与 `src/parse/mod.rs`，因此 before/after 共用完全相同的 parser、finish-time timeline/context resolution 和 DTO 定义，差别只有输入适配器：

```rust
// before：兼容 adapter
let raw = std::fs::read_to_string(path)?;
let parsed = parse_with_cwd(&raw, "raw/trace.log", Some("/bench"));

// after：64 KiB 增量 adapter
let file = std::fs::File::open(path)?;
let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
let mut parser = TraceParser::new("raw/trace.log", Some("/bench"));
let mut line = Vec::new();
while reader.read_until(b'\n', &mut line)? != 0 {
    parser.push_line(std::str::from_utf8(&line)?);
    line.clear();
}
let parsed = parser.finish();
```

每个输入由以下四行循环构成，行数均为四的整数倍并以 LF 结束：

```text
100 openat(AT_FDCWD, "/bench/root", O_RDONLY) = 3
100 access("/bench/pending", F_OK <unfinished ...>
101 openat(AT_FDCWD, "/bench/child", O_RDONLY) = 4
100 <... access resumed>) = 0
```

该形状同时覆盖普通 open、跨行 pending/resume，以及 pending 窗口内另一 PID 的事件。PID 101 未由 spawn 引入，因此两种模式都保留同一条 fail-closed identity warning；它是 parser 压力输入，不是可信 capture 样本。

| 行数 | 四行循环数 | SHA-256 |
|---:|---:|---|
| 100,000 | 25,000 | `b50023fd6c42f40be5422518f473f81783ed42576227d252c1ffe7038c9c7ea4` |
| 400,000 | 100,000 | `c4a62a3c0f7c293cba234378e0792781a199b641cfda56147be1d94b47e13571` |
| 1,200,000 | 300,000 | `6ba6287c58252482c8e8c83121ec5613819a897d5bf9d286e5757f7357345c85` |

等价检查同时保留两个 `Parsed`，先直接比较 `lines` 与完整 warning 向量，再按索引将每个 Event 序列化为 JSON bytes 比较；三个规模分别得到：

```text
equivalent lines=100000,events=75000,warnings=1
equivalent lines=400000,events=300000,warnings=1
equivalent lines=1200000,events=900000,warnings=1
```

## 构建与测量方法

临时 Cargo package 只依赖 `serde`（derive）和 `serde_json`，release profile 使用 Cargo 默认优化设置。构建命令为：

```bash
CARGO_NET_OFFLINE=true cargo build --release \
  --manifest-path target/rf600-streaming-harness/Cargo.toml \
  --target-dir target/rf600-streaming-build
```

输入由同一 harness 的 `generate <path> <lines>` 模式用 64 KiB `BufWriter` 在计时前生成。随后先运行三个规模的 `check` 模式；因此计时样本属于热 page-cache 读取，没有执行 cache drop。每个规模、每种模式各运行五次，奇数轮按 before→after、偶数轮按 after→before 交错，并固定在 CPU 0：

```bash
/usr/bin/time -f "$lines,$mode,$rep,%e,%M" \
  taskset -c 0 target/rf600-streaming-build/release/rf600-streaming-harness \
  "$mode" "target/rf600-streaming-data/trace-$lines.log" >/dev/null
```

`%e` 是 wall seconds，`%M` 是 GNU time 报告的进程 max RSS KiB。没有从样本中剔除任何一次运行。

源码与二进制指纹：

| 对象 | SHA-256 |
|---|---|
| `src/model/mod.rs` | `689ff6436ad1927303bbdaec764055bb8ae3ad4345aa4cf93c81d33fd4c7adb2` |
| `src/parse/mod.rs` | `1f4e8b77b8dd116881ee7893d43e1b75e1feeaef77a83edd33c0ca9edc68fd41` |
| 临时 harness `main.rs` | `cb928760f8f83fd978f1207684ff53ce926385508989d19150a1c4fbaa92e502` |
| release harness binary | `b594766d23f08e610a4bf552c662a1e852e148f2b87fb882bb561a8c6d3cd139` |

## 逐次结果

每列按 repetition 1～5 排列；中位数取排序后的第三项。

| 行数 | 模式 | wall seconds（5 次） | max RSS KiB（5 次） | wall 中位数 | RSS 中位数 |
|---:|---|---|---|---:|---:|
| 100,000 | before | 0.04, 0.04, 0.04, 0.04, 0.04 | 43,312, 43,208, 43,380, 43,284, 43,404 | 0.04 | 43,312 |
| 100,000 | after | 0.04, 0.04, 0.04, 0.04, 0.04 | 39,052, 38,908, 38,984, 38,828, 38,868 | 0.04 | 38,908 |
| 400,000 | before | 0.17, 0.17, 0.17, 0.17, 0.17 | 166,736, 166,844, 166,808, 166,624, 166,592 | 0.17 | 166,736 |
| 400,000 | after | 0.17, 0.16, 0.17, 0.17, 0.17 | 149,112, 149,016, 149,020, 148,956, 149,068 | 0.17 | 149,020 |
| 1,200,000 | before | 0.51, 0.52, 0.51, 0.51, 0.51 | 495,608, 495,608, 495,544, 495,644, 495,700 | 0.51 | 495,608 |
| 1,200,000 | after | 0.51, 0.51, 0.51, 0.51, 0.51 | 442,272, 442,288, 442,228, 442,496, 442,272 | 0.51 | 442,272 |

wall 改善比例按中位数计算在三组均为 0.0%；这表示 GNU time 当前报告精度下不可区分，不是“已证明 CPU 成本完全相同”。RSS 改善使用 `(before - after) / before` 的中位数计算。

## 环境

- AMD Ryzen 7 9700X 8-Core Processor，8 核 / 16 线程；进程 affinity 固定 CPU 0。
- Linux `7.2.5-200.fc44.x86_64`，x86_64；page size 4096 bytes。
- CPU governor：`performance`；测量前 load average 为 0.58 / 0.45 / 0.34，未隔离其他系统任务、未锁定频率。
- rustc `1.98.1 (48a229cea 2026-09-01)`，LLVM 22.1.8。
- GNU time 1.9；Cargo release 默认 profile。

## 局限

- 不是端到端 strace、capture 或 store 基准；不含 collector、信号监督、完成归约、JSONL 写入、fsync、checksum、readback 或 CLI 输出。
- 两种模式都必须保留完整 Event `Vec`、parser actions、timeline/context 状态直到 `finish`。增量输入只移除完整 raw `String`，因此 RSS 不会降到与最长单行相同的量级。
- 输入重复、只有两个 PID 和三个事件/四行，不代表真实 syscall、路径长度、PID fan-out 或 warning 分布。
- 计时前的等价检查预热了文件 page cache；没有测冷盘、NFS、容器限额或内存压力。
- GNU time 的 0.01 秒 wall 显示精度不足以判断这组实现是否有小幅吞吐差异；未使用硬件计数器或 allocator profiler。
- 五次样本只描述本机本次运行。历史基线因源码、构建或测量边界不同，未混入改善比例。
- 临时 harness、release build 和合成 trace 在记录数值与哈希后删除；README 不包含原始 trace 副本。
