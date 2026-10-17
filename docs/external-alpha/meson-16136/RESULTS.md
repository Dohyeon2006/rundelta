# meson-16136：实测结果

## 复现判定与已知信息

正文附带完整 Rust workspace，foo 通过相对路径依赖 bar；Cargo 可构建，Meson setup 失败。原附件按哈希验证，文件未修改；锁文件已有，离线 Cargo 构建成功。本次固定 Meson master commit 626d1e26325195f9cfdb288d7e413157b4f8bcb4，Python 3.14.7。Meson 输出：

```text
ERROR: dependency "bar-0.1-rs" was not overridden for the host machine
```

这重现了正文描述的依赖失败；原报告 master 没有指定 commit，不能证明完全相同代码版本。正常/失败分别为 Cargo build 和 Meson setup，包含不同程序与不同构建阶段，**不是同命令环境漂移实验**。

## RunDelta 独立事实

目标/tracer 退出由 0 变 1，两侧存储 verified。Cargo 有 1 条共享文件系统警告；Meson 有相同警告和 2 条 CWD 路径不确定性警告（原始行 1240、1964），两侧采集 incomplete，diff=3。它正确保留可浏览差异，不证明完整比较。

第 40–42 条读入清单；第 41 条是 `./bar/Cargo.toml` 成功打开，不能说明清单内容，也不能说明 bar 已正确注册为依赖。第 519/520/522 条对应无 `./` 的成功访问消失，路径拼写和不同命令导致表面新增/消失，不能把它们当成真实不同文件。不能仅凭字符串合并就断定物理身份。

## 推断、噪声、遗漏

前五的命令切换、sanity-check 程序和 linker 变化是事实，**均不是预登记的依赖解析线索**。Meson 标准输出比 RunDelta 更具体。内部依赖映射不在文件/exec 观察能力内；本次没有缺失 bar 文件的证据。构建入口、Python 模块加载、Rust 输出与搜索产生大量差异，无法将 856 条全算作误报。

严格命中：**否**；清单访问最早第 40 条，只是弱关联，不是“正确线索排第 40”的成功案例。定位：仅凭 RunDelta 无结论，需要目标日志和进一步语义分析。

## 产品原顺序前五

1. **Exit status** — `(metadata)`

   左：`code=Some(0), signal=None`；右：`code=Some(1), signal=None`。

2. **Capture completeness** — `(metadata)`

   左：`completed; partial; 1 warnings`；右：`completed; partial; 3 warnings`。

3. **Command arguments** — `(metadata)`

   左：`["${RUST_TOOLCHAIN}/bin/cargo", "build", "--offline", "--locked", "--manifest-path", "${WORK}/meson/fixture/meson-cargo-path-dep-reproducer/Cargo.toml", "--target-dir", "${WORK}/meson/cargo-output"]`；右：`["/usr/bin/python3", "-B", "${WORK}/meson/upstream/meson.py", "setup", "${WORK}/meson/meson-output", "${WORK}/meson/fixture/meson-cargo-path-dep-reproducer", "--wrap-mode=nodownload"]`。

4. **Executable changes** — `${WORK}/meson/meson-output/meson-private/sanity_check_for_rust.exe (execve)`

   左：``；右：`success`。

5. **Executable changes** — `/usr/bin/ld (execve)`

   左：``；右：`success`。

## 数量与证据

| 类别 | 数量 |
|---|---:|
| Exit status | 1 |
| Capture completeness | 1 |
| Command arguments | 1 |
| Executable changes | 5 |
| New file access | 677 |
| Removed file access | 171 |

共 856 条（含折叠细节）；327 条带 detail_group 标记，不等于从原始数据删除。

完整的脱敏前五及所选后排证据位置见 [summary.json](summary.json)；少量解码/脱敏 syscall 证据见 [evidence-excerpts.json](evidence-excerpts.json)。复现命令见 [REPRODUCE.md](../REPRODUCE.md)。原始日志没有随文档公开；证据行号仅指向本次隔离记录。
