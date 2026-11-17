# 批次 M 记录（复核方执行，2026-11-17）

> 分支 `dev/batch-m-20261117`，从 main（`d680d26`，即 L 复核文档提交）切出。实现提交有四个：`69f44d6`（M1–M3）、`16787d5`（M4）、`c74e94a`（M6）和 `02fdd6f`（M7）；本文随批次 M 的最终文档提交入库。
> 执行方与复核方都是 Claude，工作在云端 Linux 工作区（2 vCPU），没有接触生产服务器。计划与验收口径见 [L 复核 §5](REVIEW_2026-11-17-batchL.zh-CN.md#5-批次-m-开发计划复核方执行分支-devbatch-m-20261117)，逐项状态只写在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。下一批的起点交接是 [N 交接](HANDOFF-BATCH-N-2026-11-17.zh-CN.md)。

## 0. 结论

- M1–M4 全部完成。各项验收结果见 §3：本机门槛、CI、netem 均通过，M2 的回归测试在旧代码上失败、在新代码上通过。
- M5 第一次运行 release 深测（Linux CI）就发现了问题：
  - ak-engine 的 5 GiB 测试通过；
  - ak-itests 的 chunk sweep 在 4096 KiB / multistream 一轮卡住，600 s 后失败。本地 Windows 上历史记录过的就是这个现象；
  - 其余 ignored 测试没有运行，因为 cargo 在第一个失败的二进制处停下了。
- 复核方在本机复现了这个问题，查明是多流接收端的连接级流控死锁（P2，既有缺陷），并在 M6 修复。
- M6 之后第二次运行：chunk sweep 全部通过，但 `property_random` 的中断续传项 seed 1 失败。这次揭出两件事，都在 M7 修复：
  - 一个过时的测试断言：wave 17 之后，报告里的任务号与检查点记录号已经不是同一个值；
  - 一个既有缺陷（P3）：被取消的多流接收会留下仍持有流句柄的任务，发送方因此要等满 30 s 空闲才失败。本地 Windows 上历史记录的 `IdleTimeout(30s)` 就是它，复核方已在 Linux 上复现。
- M7 之后的第三次运行把全部 ignored 深测都跑到了：
  - 除 65,537 文件的边界项之外全部通过。netem 基准打印 SKIP，因为 runner 不能整形网卡，不算执行；
  - 65,537 文件这一项在接收端报 `Too many open files`，揭出一个新缺陷 M-N1：接收端在应答 ResumeRequest 之前，为每个文件各打开一个 part 文件并一直持有。
  - 复核方在本机复现：把软上限设为 1024（常见的 Linux 默认值）时，2,000 个文件的常规测试就会失败；把上限放宽到 4096 后，同一测试通过。
  - M-N1 涉及文件写入核心和符号链接竞态防护，本批没有修，列为批次 N 的 N4。
- 留给本地 agent 的部分写在 N 交接：Windows 上反复运行确认 M2、Windows release 深测、删除已合入的远端分支。

## 1. 交付

| 项 | 提交 | 改动 |
|---|---|---|
| M1 | `69f44d6` | `check_repository.status_table_issues` 检查第 7 节表格的每一条数据行，表头和分隔行按位置排除，不再按编号前缀筛选；一个以竖线开头、却不属于任何表格的行（被空行或文字隔开）报为表外行。新增 2 个单测 |
| M2 | `69f44d6` | `framing::write_frame` 在 `write_all` 后 `flush`；`client_v2::conn` 新增 `send_encoded`（`write_all` + `flush`），Hello 写入与写任务都改用它；集成测试中三处直接写 TLS 流的地方补 `flush`；新增仅测试用的 `test_support::gated_tls_pair`，以及 2 个回归测试；`TestClient` 增加 `label`（`名字@本地地址`），超时报错时带上它 |
| M3 | `69f44d6` | runner 先捕获命名空间内 `sysctl -w` 的退出码与 stderr，再读回两个上限，并以读回值为准报告：上限有效且写入被接受、上限有效但写入被拒绝或无法确认（此时值来自主机层面的提升）、上限没有提升（WARNING）。合同测试的 mock 增加对应开关，新增 1 个测试。`bench_netem` 开启 `ak_net=info` 日志；ak-itests 增加 `env_logger` 开发依赖，`Cargo.lock` 只多出这一条依赖边 |
| M4 | `16787d5` | 三个测试模块移到单独文件（见 §2.3）；新增独立开发工具 `scripts/tokcmp` |
| M5 | 手动触发的 CI | 首次运行 `release-deep-tests`，结果见 §3 |
| M6 | `c74e94a` | 多流接收不再在驱动循环里同步读新流的第一帧，改由每条流自己的开流任务读取；多流接收失败时显式关闭连接（关闭码 `0xA003`）；新增 2 个回归测试（见 §2.4） |
| M7 | `02fdd6f` | `ReceiveTaskGuard`：多流接收失败或被取消时，中止它的全部辅助任务（控制流读取、监控、开流、每文件接收），完成时解除；新增 1 个回归测试。`property_random` 中断续传项改为按 wave 17 之后的语义断言（见 §2.5） |

## 2. 设计说明

### 2.1 为什么是 flush，而不是放宽超时（M2）

- L-N3 那次失败的时间线排除了 CPU 抢占：超时窗口的最后约 4 s 里机器空闲，连接也没有断开。见 L 交接 §9。
- tokio-rustls 0.26.6 的 `poll_write` 在底层写入 Pending 时，只要明文已收进会话，就返回 `Ready(Ok(n))`。只有 `poll_flush` 会一直等到加密记录真正交给传输层。
- 所以，帧写入的“完成”必须包含 flush。这一点同时修复了生产代码：服务端的全部帧写入，以及客户端的 Hello 与写任务。
- fair-share 测试的 10 s 与 2 s 上限保持不变。

### 2.2 netem 的三种报告（M3）

| 情况 | runner 输出 |
|---|---|
| 命名空间内写入成功，读回 16 MiB | `UDP buffer ceilings effective in the namespace (CR-P3-19): rmem_max=16777216 wmem_max=16777216; namespace write accepted` |
| 写入被拒或有 stderr，但读回 16 MiB | `… namespace write refused or unconfirmed (rc=…; stderr: …), so the value is the host's (global-sysctl kernel; raised by the workflow step)` |
| 读回不是 16 MiB | `WARNING: UDP buffer ceilings NOT raised in the namespace (CR-P3-19): …; this run measures capped socket buffers` |

最后一种情况不会让 runner 失败：测得的仍是一组有效数据，只是对应受限的缓冲，日志会明确写出来。

### 2.3 纯移动与证明（M4）

| 原文件 | 新文件 | 原文件保留的行 |
|---|---|---|
| `crates/ak-ffi/src/task.rs`（`mod task_tests`） | `crates/ak-ffi/src/task/task_tests.rs` | 第 1–4250 行 |
| `crates/ak-cli/src/main.rs`（`mod tests`） | `crates/ak-cli/src/tests.rs` | 第 1–4625 行 |
| `crates/ak-signal/src/server_v2/engine.rs`（`mod tests`） | `crates/ak-signal/src/server_v2/engine/tests.rs` | 第 1–3447 行 |

证明分三层，方法见 N 交接 §2.3：

1. **字节**：原文件保留的部分移动前后字节相同；新文件等于旧模块体去掉 4 格缩进后经 rustfmt 格式化的结果。
2. **记号**：用 `scripts/tokcmp` 比对。
3. **测试名单**：移动前后一致。

`tokcmp` 只容忍 rustfmt 造成的、不改变含义的差异。在这三个文件里，这类差异是：

- `\` 续行的字符串字面量缩进少了 4 列——Rust 词法会吞掉续行的前导空白，字符串的值不变；
- 一处单参数调用的尾逗号被 rustfmt 去掉。

反向检查：故意改动一个字符串字面量、删掉一条语句、改动父文件中的一个常量，`tokcmp` 都报告不同。用 `tokcmp` 复查批次 L 的 L2 移动，结果也一致：唯一的原始记号差异是 `fake_sender_side` 参数表中一个被 rustfmt 去掉的尾逗号。

### 2.4 多流接收的流控死锁（M6）

**机制**

- 旧的 `multistream_receive` 每接受一条新流，就在驱动循环里同步等这条流的第一帧，等到才去接受下一条。
- 发送方是同时打开多条流（最多 4 条）一起发的。还没被接受的流上已经到达的数据，照样占用 QUIC 的连接级接收窗口（8 MiB），而窗口额度只有在应用读走数据之后才会归还。
- 于是，当“并发流数 ×（块大小 + 帧头）”超过窗口时，正在等的那一帧永远凑不齐：
  - 2 条流、4 MiB 的块就会卡住；
  - 4 条流、2 MiB 的块也会卡住；
  - 默认的 1 MiB 块、4 条流不会卡住。
- 旧代码里还有第二条可能的路径：驱动阻塞期间不再处理事件，容量 64 的事件通道写满后，其他流的接收任务也会停止读取，同样会占住窗口。这条路径没有单独复现。

**影响范围**

- FFI 的发送接口接受 `chunk_kb`（1–8192）和 `multistream`，CLI 的 bench 也接受，所以不是默认配置的参数组合也能走到这里。
- 一旦卡住，发送方会一直等下去，而且得不到任何失败信号：接收方的控制流读取任务（按设计永不取消）一直持有流句柄，QUIC 连接靠保活 PING 一直不断。

**修复**

1. 第一帧改由每条流自己的开流任务读取，再用 `StreamEvent::Opened` 交回驱动。驱动随时可以接受新流、处理事件，所有流同时被读取。
2. 多流接收失败时，接收方用关闭码 `0xA003`（`MULTISTREAM_ABORT_CLOSE_CODE`）显式关闭连接，发送方立即得到失败。单流路径不需要这样做：失败时它的句柄会全部随调用一起释放。

线格式与成功路径的行为不变。

**回归测试**（ak-engine）

| 测试 | 内容 | 在旧代码上的结果 |
|---|---|---|
| `multistream_chunks_beyond_the_connection_window_share_complete` | 3 个 8 MiB 文件，4 MiB 块，多流 | 60 s 内完不成 |
| `multistream_receive_failure_closes_the_connection` | 发送方只盯着连接，不主动断开 | 连接一直开着 |

只去掉显式关闭时，后一个测试同样失败。

### 2.5 被取消的多流接收与过时断言（M7）

**缺陷（P3，既有）**

- 测试夹具用“中止接收任务”来模拟进程重启。多流接收的辅助任务不会随之结束：控制流读取任务按设计永不取消，每文件接收任务也还在。
- 这些任务继续持有流句柄，连接靠保活 PING 一直不断。发送方只能等满自己的空闲预算（30 s），以 `IdleTimeout(30s)` 失败；如果当时正卡在受流控的写入里，就会一直等下去。
- 本地 Windows 的 release 深测多次在中断续传项的 seed 1 这样失败。复核方在 Linux 上用 seed 7 复现了同一失败。

**修复**

- `multistream_receive` 用 `ReceiveTaskGuard` 记下它派生的每个任务。
- 函数因失败返回、或者整个 future 被丢弃时，guard 中止所有仍在运行的任务。最后的句柄释放后，quinn 隐式关闭连接，发送方立即失败。
- 接收完成时 guard 解除，任务的行为与以前完全相同：控制流读取任务继续排空，直到 FinalAck 的等待结束。
- 每完成一个文件，guard 清掉已结束的任务句柄，长任务也只保留活着的任务。

**过时断言**

- wave 17（`331c80b`，`record_id_for`）把检查点的记录号与线上的内容号分开了：`Report.job_id` 是内容号，`.akjob` 用的是记录号（内容号 + 对端 + 方向 + 根）。
- 中断续传项仍拿报告里的任务号去比记录号，这个比较从 wave 17 起就不可能成立。Windows 上先在 leg 1 失败，所以一直没有跑到这一步；Linux 上第一次跑到了。
- 现在的断言是：
  - 两端报告同一个内容号；
  - leg 2 推导出的记录号正好是 leg 1 留下的那条记录（同一传输实例，所以续传的就是它）；
  - 两端都跳过了已验证的块（原有断言）。

**回归测试**

`cancelled_multistream_receive_closes_the_connection`：假发送方进入数据阶段后中止接收任务，自己保持所有句柄，检查连接是否被关闭。没有 guard 时连接一直开着。

## 3. 证据表

本机门槛在云端 Linux 工作区（2 vCPU）运行，rustc 1.97.0。`rust-toolchain.toml` 钉的 1.99.1 无法下载（代理拒绝访问 static.rust-lang.org），与 CI 版本一致的结论以 CI 为准。

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
| L 复核文档提交的 CI（dev） | `d680d26` run [43986079793](https://github.com/wmqfl861/AK-sending/actions/runs/43986079793) | 第 1 次运行：12 项 success，1 项 skipped（手动深测） |
| L 复核文档提交的 CI（main） | `d680d26` run [43988139271](https://github.com/wmqfl861/AK-sending/actions/runs/43988139271) | 由快进推送 main 触发。第 1 次运行：12 项 success，1 项 skipped |
| L 复核文档提交的 netem（main） | `d680d26` run [43988139275](https://github.com/wmqfl861/AK-sending/actions/runs/43988139275) | success。main 从 `9e8c36a` 前进到 `d680d26`，包含 L3 的路径改动，因此触发；runner 仍是旧版，打印 “namespace UDP buffer ceilings raised”。100 ms 档、8 MiB 窗口：60.4 RTT，1 次拥塞事件，丢包 10；四档 RcvbufErrors 均为 +0 |
| M2 机制与回归 | `69f44d6` | `framing::tests::a_written_frame_reaches_the_peer_through_tls_without_a_later_write` 在旧 `write_frame` 上失败，报 framing.rs:210 “write_frame reported success for a frame still buffered inside TLS”。`client_v2::conn::tests::send_encoded_returns_only_after_the_frame_left_tls` 在临时去掉 `send_encoded` 的 flush 时失败，报 conn.rs:408，恢复 flush 后通过。机制实验见 L 交接 §9 |
| M1–M3 本机门槛 | `69f44d6` 的树 | fmt 通过；clippy `-D warnings` 通过；rustdoc `-D warnings` 通过；workspace 测试 68 组结果，1204 passed / 0 failed / 8 ignored（比 L 多出 M2 的 2 个）；ak-store `sqlite` 52 passed；ak-signal `server-persistence` 115 + 26 + 53 + 26 + 1 passed；Python 366 项 OK（多出 M1 的 2 个、M3 的 1 个）；`check_repository.py` passed（新检查器，状态表 87 行）；`check_contracts.py` 5/5 PASS；部署自检 PASS；跨 NAT 自检 PASS；耗时 788 s |
| M4 纯移动证明 | `16787d5` 对 `69f44d6` | ak-ffi：保留第 1–4250 行（字节相同），模块体 4266 行 → 新文件 4256 行，109 个条目；ak-cli：保留第 1–4625 行，3148 → 3143 行，107 个条目；ak-signal：保留第 1–3447 行，2417 → 2414 行，48 个条目。三个文件都满足：rustfmt（去缩进的旧模块体）与新文件字节相同；`tokcmp` 的模块体与父文件比对均为 true；测试名单前后一致，ak-cli 145 个（其中 `tests::` 84 个）、ak-ffi 64 个（`task::task_tests::` 57 个）、ak-signal 115 个（`server_v2::engine::tests::` 34 个）。规范化之前原始记号不同的条目：ak-ffi 10 个（9 个是 `\` 续行字符串缩进少了 4 列，1 个是单参数调用的尾逗号被 rustfmt 去掉），ak-cli 3 个、ak-signal 4 个（都是续行字符串） |
| M4 本机门槛 | `16787d5` 的树 | 与上一行同一组门槛全部通过；workspace 测试 1204 passed / 0 failed / 8 ignored；Python 366 项 OK；状态表 87 行；耗时 650 s |
| 批次 M 推送的 CI | `16787d5` run [43990607099](https://github.com/wmqfl861/AK-sending/actions/runs/43990607099) | 第 1 次运行：12 项 success，1 项 skipped（手动深测）。其中 Windows Rust 任务跑了一次 ak-signal 全部测试并通过，这只是一次，不能替代 N1 的反复运行 |
| 批次 M 推送的 netem | `16787d5` run [43990607171](https://github.com/wmqfl861/AK-sending/actions/runs/43990607171) | success。runner 输出 “UDP buffer ceilings effective in the namespace (CR-P3-19): rmem_max=16777216 wmem_max=16777216; namespace write refused or unconfirmed (rc=0; stderr: sysctl: setting key "net.core.rmem_max": Operation not permitted \| sysctl: setting key "net.core.wmem_max": Operation not permitted), so the value is the host's (global-sysctl kernel; raised by the workflow step)”。ak-net 日志共 8 行（四档 × 两端），都是 `SO_RCVBUF=Some(16777216) SO_SNDBUF=Some(16777216)`（Linux 把 8 MiB 的请求翻倍报告）。四档 RcvbufErrors 均为 +0。100 ms 档、8 MiB 窗口：80.5 RTT，2 次拥塞事件，丢包 16，lo 无丢包 |
| 8 MiB 档的波动 | 上面三次 netem 运行 | 100 ms 档的 RTT 数依次是 54.8（`2bc6a87`）、60.4（main 上的 `d680d26`）、80.5（`16787d5`），拥塞事件 1 / 1 / 2 次，丢包 10 / 10 / 16。三次的 RcvbufErrors 与 lo 丢包都是 0，所以剩余差距和这次波动都不是接收缓冲溢出造成的。原因没有定位，本批不追 |
| M5 第一次运行 | `16787d5` 手动触发 run [44025654906](https://github.com/wmqfl861/AK-sending/actions/runs/44025654906) | 12 项 success，1 项 failure，即 release-deep-tests：ak-engine `five_gib_sparse_file_transfers_with_u64_offsets` 通过（37.06 s）；ak-itests 的 `bench_chunk_sweep` 前 7 轮各用 1.5–2.0 s 完成，第 8 轮 4096KiB/multistream 在 common/mod.rs:362 报 “loopback transfer did not finish within 600s”，该测试用时 613.48 s；cargo 随即停止，排在它后面的 ignored 测试二进制没有运行 |
| M6 本机复现 | 本机 debug，旧 `16787d5` 的引擎；一次性复现测试，不入库 | ak-itests 夹具，窗口 8，每个文件 16 MiB。多流时：4 个文件、4096 KiB 块 → 150 s 预算内完不成；1 个文件、4096 KiB → 0.27 s 完成；2 个、4096 KiB → 60 s 内完不成；3 个、4096 KiB → 完不成；4 个、2048 KiB → 完不成；3 个、2048 KiB → 0.72 s；4 个、1536 KiB → 0.92 s；4 个、1024 KiB → 0.92 s |
| M6 修复后 | 本机 debug，`c74e94a` 的引擎 | 同一复现测试：4×4096 用 1.00 s、2×4096 用 0.46 s、4×2048 用 0.93 s、4×1024 用 0.86 s、4×8192（多流）用 0.95 s、1×8192（单流）用 0.28 s、4×6144 用 0.89 s，均完成且字节数一致。两个新回归测试在旧 lib.rs 上都失败：一个报 “the transfer must finish, not deadlock on connection flow control: Elapsed(())”（60 s），另一个报 “the failed multistream receive must close the connection, not leave the sender waiting”；只去掉显式关闭时，后者同样失败；完整修复下 ak-engine 的 multistream 测试 8 passed / 0 failed |
| M6 本机门槛 | `c74e94a` 的树 | 同一组门槛全部通过；workspace 测试 68 组，1206 passed / 0 failed / 8 ignored（多出 M6 的 2 个）；Python 366 项 OK；状态表 88 行；耗时 713 s |
| M6 推送 | `c74e94a` run [44030860383](https://github.com/wmqfl861/AK-sending/actions/runs/44030860383) | 推送触发的 CI 被紧接着手动触发的运行按并发规则取消；手动运行包含全部任务，见下一行 |
| M5 第二次运行（M6 之后） | `c74e94a` 手动触发 run [44030860114](https://github.com/wmqfl861/AK-sending/actions/runs/44030860114) | 12 项 success，1 项 failure，即 release-deep-tests：ak-engine 5 GiB 通过（42.17 s）。`bench_chunk_sweep` 8 轮全部完成，4096KiB/multistream 用 2.11 s，该测试共 19.04 s。`bench_netem` 打印 SKIP（runner 没有 CAP_NET_ADMIN，属预期）。`bench_throughput` 512 MiB×2 通过（11.00 s）。`property_random` 的 25 种子传输项通过（6.13 s），10 种子中断续传项在 seed 1 失败：property_random.rs:295 报 “job id must survive the interruption (resume, not a new job)”，左边是内容号，右边是记录号。cargo 随即停止，20,000 / 65,537 文件两项没有运行 |
| M7 本机复现与验证 | 本机 debug | 只修正断言、还没有 guard 时：seed 1–6 通过，seed 7（多流、256 KiB）在 property_random.rs:234 报 “leg-1 sender must fail closed after the mid-data abort, got Err(IdleTimeout(30s))”，与 Windows 上的历史失败相同。新测试 `cancelled_multistream_receive_closes_the_connection` 在 `c74e94a` 的 lib.rs 上失败（5.07 s），加上 guard 后通过；ak-engine 的 multistream 测试 9 passed / 0 failed；`property_random` 的完整 ignored 套件连续跑两次，都是 2 passed / 0 failed（中断续传项 2.74 s / 2.59 s，传输项 8.58 s / 8.54 s） |
| M7 本机门槛 | `02fdd6f` 的树 | 同一组门槛全部通过；workspace 测试 68 组，1207 passed / 0 failed / 8 ignored（多出 M7 的 1 个）；Python 366 项 OK；状态表 88 行；耗时 658 s |
| M6、M7 推送 | `c74e94a` run [44030860383](https://github.com/wmqfl861/AK-sending/actions/runs/44030860383)；`02fdd6f` run [44033739862](https://github.com/wmqfl861/AK-sending/actions/runs/44033739862) | 两次推送触发的 CI 都被紧接着手动触发的运行按并发规则取消；手动运行包含全部任务 |
| M5 第三次运行（M7 之后） | `02fdd6f` 手动触发 run [44033739677](https://github.com/wmqfl861/AK-sending/actions/runs/44033739677) | 12 项 success，1 项 failure，即 release-deep-tests。这一次跑到了全部 ignored 测试二进制。<br>通过的：ak-engine 5 GiB（28.77 s）；`bench_chunk_sweep` 8 轮（4096KiB/multistream 用 0.95 s，该测试 9.87 s）；`bench_throughput`（5.44 s）；`property_random` 两项（中断续传 10 种子 0.38 s，传输 25 种子 1.41 s）；`t13_many_files` 的 20,000 文件项（21,200 个条目，83 页清单，80.6 s）。<br>SKIP：`bench_netem`（不算执行）。<br>失败的：65,537 文件边界项，t13_many_files.rs:237 报 `send=Err(Stream(Connection("connection lost"))) recv=Err(Io(Os { code: 24, kind: TooManyOpenFiles, message: "Too many open files" }))`，即 M-N1 |
| M-N1 本机复现 | 本机 debug，`02fdd6f` | 常规（非 ignored）测试 `two_thousand_small_files_transfer_byte_identical_with_paging`：在 `ulimit -n 1024` 下失败（“send failed: Stream(Connection("connection lost"))”，0.33 s）；在 `ulimit -n 4096` 下通过（6.30 s）。注意这个测试的收发两端在同一进程里，共享同一个上限；CI 上 65,537 文件那一项的失败信息直接指向接收端 |

