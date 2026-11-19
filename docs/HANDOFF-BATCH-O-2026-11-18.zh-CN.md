# 交接文档：批次 O（2026-11-18）

> 起点：main 快进到批次 N 复核的最终文档提交（本文随该提交入库）。批次 N 的结论见 [N 复核](REVIEW_2026-11-18-batchN.zh-CN.md)；复核方的命令、运行编号与计数见 [N 交接 §9](HANDOFF-BATCH-N-2026-11-17.zh-CN.md#9-复核方证据2026-11-18)。执行方从本文 §8 证据表开始记录本批的运行编号与计数。
> 每项状态只写在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

## 0. 一句话现状

批次 N 按交付形态没有通过复核：N4 没有修好 M-N1，CI 回归门槛被改成区分不了新旧代码的步骤；N2 里 65,537 文件项的失败是一个更大的既有缺陷 N-N1 的表现。复核方在同一分支上补完 M-N1、修复了 N-N1，再整批合入 main。

批次 O 的工作都要在本地 Windows 上做，因为复核方的改动只在 Linux 上验证过：

- O0：收尾清理；
- O1：Windows 常规测试，确认复核方的改动；
- O2：Windows release 深测，逐项记录，65,537 文件项必须通过；
- O3：可选的拆分工程项（原 N3）。

## 1. 复核方在批次 N 分支上的改动

| 项 | 内容 | 主要位置 |
|---|---|---|
| M-N1 补完 | 文件最后一块写完即释放 part 文件句柄（N4 的句柄要开到 `finish`，而引擎在整个块阶段之后才逐个 `finish`，所以每个收到内容的文件仍各占一个描述符）。Windows 的文件身份查询改用 winapi-util 的安全封装，`ak-storage` 恢复 `#![forbid(unsafe_code)]` | `crates/ak-storage/src/lib.rs`（`PartState`、`write_chunk`、`file_identity`）、`crates/ak-storage/Cargo.toml` |
| M-N1 回归门槛 | CI 的 Linux Rust 任务先在默认上限下编译，再在 `ulimit -n 256` 下跑 2,000 文件测试；批次 N 的代码在这一步失败，新代码通过 | `.github/workflows/ci.yml` |
| N-N1 | 接收端活动信号：准备阶段边准备边发 ResumeRequest 分片；校验阶段向协商了 `CAP_PAUSE_RESUME` 的发送端发无操作的 `Control::Resume`；校验阶段的检查点改为开头保存一次、此后至多每秒一次 | `crates/ak-engine/src/lib.rs`（`ResumeShards`、`liveness_interval`、`receive_frozen_body`）、`docs/PROTOCOL.zh-CN.md` |
| N-N2 | t13 测试改在多线程运行时上跑；新增多线程回归测试 `slow_verify_phase_keeps_the_sender_waiting`；规模测试输出各阶段时间线（`phases:` 一行） | `crates/ak-itests/tests/t13_many_files.rs`、`crates/ak-itests/tests/common/mod.rs`、`crates/ak-engine/src/tests.rs` |

## 2. 接手前必读

### 2.1 part 文件句柄（M-N1）

- prepare 之后 sink 不持有句柄；一个会话里第一次写入时打开，写完该文件的最后一块就释放；`finish` 重新打开做整文件摘要校验，提交前再释放。
- 每次重新打开都会先查符号链接、再核对文件身份（Unix 是设备号与 inode，Windows 是卷序列号与文件索引）和长度，不符即失败。
- 身份核对是纵深防护：Linux 文件系统可能把被删 part 文件的 inode 号分给新文件，同长度的替换文件能通过这两项检查。挡住未验证内容的是 `finish` 的整文件摘要。
- 改接收路径时，不要让任何 sink 在“块写完”与“`finish`”之间持有句柄：引擎在整个块阶段之后才进入校验循环。

### 2.2 接收端活动信号（N-N1）

- 发送端等每一帧都受空闲时限约束（默认 30 s）。接收端有两段工作不发帧，而且随文件数增长：应答 ResumeRequest 前的准备，以及 Finalize 之后的逐文件校验与提交。
- 现在接收端在这两段里至多每隔 min(本端空闲时限/4, 5 s) 发一帧：准备阶段发 `more = true` 的 ResumeRequest 分片（可以为空），校验阶段发无操作的 `Control::Resume`。没有新增消息类型，线缆布局不变。
- 单个文件本身的校验（例如超大文件的整文件哈希）如果超过发送端的空闲时限，不在这个信号覆盖范围内。
- itests 的单线程运行时会掩盖这类缺陷：接收端阻塞的准备或校验过程会连带停住发送端的计时。验证活动信号或超时行为的测试要用多线程运行时。

### 2.3 规模测试的阶段时间线

`t13_many_files` 的规模测试（4,097、20,000、65,537）先打印一行 `T13-…: phases: …`，列出 manifest received、first chunk sent、last chunk received、verify phase started、final ack、done 的时刻（final ack 与 done 收发两端各记一次，所以各出现两次）。失败时凭这一行判断是哪一段超出了哪一个时限。复核方在 Linux CI 上取得的两行原文见 N 交接 §9，可作对照。

### 2.4 纯移动的证明方法（O3 会用到）

见 [N 交接 §2.3](HANDOFF-BATCH-N-2026-11-17.zh-CN.md#23-纯移动的证明方法m4n3-会用到)。

## 3. 全项目未完成事项

### 3.1 所有者事项（agent 不得代办）

与 [N 交接 §3.1](HANDOFF-BATCH-N-2026-11-17.zh-CN.md#31-所有者事项agent-不得代办) 相同：E6 生产部署、C4 物理双机跨 NAT、E5 签名、E7 许可证、CR-P3-03、CR-P3-05。批次 M 的 M2 与本轮的改动都要等 E6 部署后才进入生产服务器。

### 3.2 工程项

- O1、O2：本批。
- CR-P2-08：生产代码的拆分（O3，可选）。
- 复核登记的两个后续工程项（海量文件的块阶段检查点开销、超大单文件的接收端活动信号）记在 [BACKLOG](BACKLOG.zh-CN.md) 的 P3-02 之下。
- `raise_nofile_soft_limit_to_hard` 在 macOS 上（硬上限报告为无限）不起作用；接收端只需少量描述符，不影响功能。
- CR-P3-19 的剩余差距、Linux GUI、Android arm64 真机、BACKLOG 的 P3/P4：同 N 交接 §3.2。

## 4. 批次 O 工作单

| 项 | 步骤 | 验收 |
|---|---|---|
| O0 | 1) `git fetch --prune`；2) 确认 `git merge-base --is-ancestor origin/dev/batch-n-20261117 origin/main` 成立，然后删除这个远端分支；3) 切到 main 并 `git pull --ff-only`，删除本地旧分支，执行 `git worktree prune`；4) 从 main 切出 `dev/batch-o-<日期>`；5) 在 §8 回填 main 上本文所在提交的 CI 运行编号与结论 | `git branch -a` 只剩 main 和本批分支；`git worktree list` 只剩主目录；§8 有回填行 |
| O1 | Windows 常规确认：1) `cargo test --locked --workspace --no-fail-fast` 完整运行一次；2) 用 PowerShell 循环连续运行 10 次 `cargo test --locked -p ak-engine --lib slow_verify_phase_keeps_the_sender_waiting`，每次记录退出码；3) 单独运行一次 `cargo test --locked -p ak-storage`，确认 Windows 上的身份核对测试（`replaced_part_file_fails_closed_on_write_and_finish` 应报 `PART_REPLACED`） | 全部通过；失败时保留完整输出、查清根因，不放宽任何超时或断言 |
| O2 | Windows release 深测：运行 `cargo test --locked --release -p ak-engine -- --ignored --nocapture` 与 `cargo test --locked --release -p ak-itests -- --ignored --nocapture --test-threads=1`（规模测试串行，避免互相争抢磁盘）。§8 逐项记录每一个 ignored 测试的名字、结论与耗时，包括 chunk sweep 的 4096 KiB / multistream（M6 的 Windows 确认点）、throughput、两项 property_random、20,000 文件与 65,537 文件；两个规模测试还要原样贴上 `phases:` 那一行 | 65,537 文件项通过；Windows 结论与复核方的 Linux 结论分开记录；失败时保留完整输出与 `phases:` 行，判断是哪一段超时，不放宽超时 |
| O3（可选） | CR-P2-08 生产代码拆分，做法同 [N 交接 §4 的 N3 行](HANDOFF-BATCH-N-2026-11-17.zh-CN.md#4-批次-n-工作单)：两步、两个提交，第二步按 §2.4 证明是纯移动 | 测试名单不变；clippy/rustdoc 通过；公开 API 不变 |

并行建议：O1 与 O2 都要独占 Windows 机器的 CPU 与磁盘，串行做，先 O1 后 O2。O3 只改代码结构，可以由另一个执行者并行做，但不要与改接收路径的工作同时进行。

## 5. 门槛与命令

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --no-fail-fast
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
cargo test --locked -p ak-store --features sqlite
cargo test --locked -p ak-signal --features server-persistence
python -m unittest discover -s scripts/tests -v
python scripts/check_repository.py
python scripts/check_contracts.py
bash -n scripts/deploy_linux.sh && bash scripts/deploy_linux.sh --self-test
bash scripts/cross_nat_test.sh --self-test
```

O1 的 PowerShell 循环示例：

```powershell
$results = 1..10 | ForEach-Object {
  cargo test --locked -p ak-engine --lib slow_verify_phase_keeps_the_sender_waiting *> "o1-run-$_.log"
  [pscustomobject]@{ Run = $_; ExitCode = $LASTEXITCODE }
}
$results | Format-Table
```

补充说明：

- 本机 Rust 应使用 rust-toolchain.toml 钉的 1.99.1，与 CI 一致；改用其他版本时如实记录。
- 循环与深测产生的日志不入库，只在 §8 写次数、结果、耗时和失败时的报错行。
- 缺工具时写“未执行”，不能写成通过。

## 6. 文档与分支规则（摘自 AGENTS.md）

- 每项状态只写在状态表里，提交列写真实提交号，不写“本提交”；`check_repository.py` 会检查状态表的每一行，也会拦下被空行隔到表外的行。状态表的“证据摘要”列不写运行编号和计数。
- run 号和计数只写在本文 §8；STATUS 只写阶段结论、下一步和链接，并链接到本文。
- 本文不要同时出现“已核”与“尚待 / 待补 / 仍待 / 待执行”（`check_repository.py` 会拦截）。
- 执行方只推 `dev/batch-o-<日期>`；复核通过后由复核方快进合入 main，再由执行方删除分支。任何人都不强推，包括改写自己刚推上去的提交：发现错误就追加一个新提交。
- 新增的 CI 门槛如果失败，先查清原因；不要为了让它通过而削弱它。确实要改门槛时，在提交说明里写明理由，并给出它仍能区分新旧代码的证据。

## 7. 红线

- 不引入 TURN/relay，服务器不经手文件数据（ADR-001）。
- 不部署、不登录、不修改生产服务器，也不改它的防火墙。
- 不生成签名密钥。
- 配对密钥、令牌、用户文件、真实文件名或路径、完整网络诊断都不入库。
- 模拟或同机测试不写成真机验证。

## 8. 证据表（执行方填写）

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
| main 上批次 N 最终提交的 CI | `e8c7d84` run [44146280956](https://github.com/wmqfl861/AK-sending/actions/runs/44146280956) | completed success（12 项常规任务 success，1 项手动深测 skipped） |
| O1 workspace | 本地 Windows | **1219 passed, 0 failed, 8 ignored** |
| O1 slow_verify 10 次 | 本地 Windows | **10/10 通过，0 失败** |
| O1 ak-storage | 本地 Windows | **57 passed, 0 failed**；`replaced_part_file_fails_closed_on_write_and_finish` 通过 |
| O2 ak-engine 5 GiB | 本地 Windows | **通过**（226.68s）；磁盘 6.8GB 时两次失败（connection lost），清理至 23GB 后通过 |
| O2 chunk_sweep_256mib | 本地 Windows | **通过**（36.88s） |
| O2 netem 50/100ms | 本地 Windows | SKIP（tc netem 仅 Linux；同机 Windows 不适用） |
| O2 multistream_512mib | 本地 Windows | **通过**（20.03s）——M6 Windows 确认 |
| O2 property_random 10 seeds | 本地 Windows | **通过**（含 seed 1 中断续传）——M7 Windows 确认 |
| O2 property_random 25 seeds | 本地 Windows | **通过** |
| O2 20,000 文件 | 本地 Windows | **通过**（369.1s）；`T13-20000: phases: manifest received @ 9.0s, first chunk sent @ 26.5s, last chunk received @ 303.7s, verify phase started @ 306.7s, final ack @ 369.1s, final ack @ 369.1s, done @ 369.1s, done @ 369.2s` |
| O2 65,537 文件 | 本地 Windows | **通过**（2366.2s ≈ 39.4 min）；`T13-65537: phases: manifest received @ 26.1s, first chunk sent @ 67.7s, last chunk received @ 2143.1s, verify phase started @ 2151.8s, final ack @ 2366.1s, final ack @ 2366.1s, done @ 2366.2s, done @ 2366.3s`——**首次在 Windows 上通过** |
| O2 全量门槛 | 本地 Windows | fmt/clippy/rustdoc/sqlite/server-persistence/Python/repo/contracts/部署自检/跨NAT自检 全部通过 |
| O 批次最终 CI | `4f0331c` run [44242047799](https://github.com/wmqfl861/AK-sending/actions/runs/44242047799) | **12/12 常规任务 success**，1 项手动深测 skipped |

## 9. 复核方证据（2026-11-19）

复核方是 Claude，在云端 Linux 工作区（2 vCPU，7 GB 内存，ext4，virtio 磁盘）操作，没有接触生产服务器，也没有 Windows 环境。结论见 [O 复核](REVIEW_2026-11-19-batchO.zh-CN.md)，逐项状态只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。“探针”指不入库的一次性测试程序；除注明外都是 release 构建、多线程运行时（两个工作线程）、两端同机环回。

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
| 远端分支与推送记录 | GitHub API（branches、activity），2026-11-19 | 远端只有 `main` 与 `dev/batch-o-20261118`。`dev/batch-n-20261117`：2026-11-18T02:33:13Z `branch_deletion`。`dev/batch-o-20261118`：05:07:45Z `branch_creation` → `4f0331c`，05:22:05Z `push` `4f0331c` → `7679303`；没有强推 |
| §8 引用的运行与最终提交的 CI | run [44146280956](https://github.com/wmqfl861/AK-sending/actions/runs/44146280956)、[44242047799](https://github.com/wmqfl861/AK-sending/actions/runs/44242047799)、[44243762934](https://github.com/wmqfl861/AK-sending/actions/runs/44243762934) | 分别是 `e8c7d84`（main）、`4f0331c`、`7679303` 的 push 运行，都是 completed success（API 核对） |
| O2 的 Windows 阶段时长（由 §8 的两行时间线相减） | 执行方数据，复核方计算 | 20,000 文件：准备阶段（manifest received → first chunk sent）17.5 s；块阶段（→ last chunk received）277.2 s，平均每个文件 13.9 ms；发送端核对源文件并发 Finalize（→ verify phase started）3.0 s；校验阶段（→ final ack）62.4 s。65,537 文件：准备 41.6 s；块阶段 2,075.4 s，平均每个文件 31.7 ms；核对源文件 8.7 s；校验 214.3 s。准备阶段与校验阶段长于 30 s，说明 N-N1 的两半修复在 Windows 上都是必需的 |
| O-N1 复现（批次 O 代码） | `7679303`，探针，一个稀疏的大文件 | 5 GiB、默认 30 s：两端成功，校验阶段 2.7 s；12 GiB、默认 30 s：两端成功，校验阶段 10.2 s（这台机器磁盘快，没有超过默认时限）。12 GiB、两端时限 3 s：`send=Err(IdleTimeout(3s))`，`recv=Ok(12884901901)`，校验阶段 45.92–56.52 s。先传 25 s 后中断（目标目录已有 5.63 GB），再以 3 s 续传：准备阶段 7.51–9.37 s，校验阶段 31.88–42.57 s，`send=Err(IdleTimeout(3s))`，接收端成功 |
| O-N1 修复之后 | `f9384b9` 的改动（探针跑在提交前的工作树上，与提交只差活动信号发送失败时的处理），探针 | 同样的 12 GiB、3 s：两端成功（校验阶段 45.50–55.78 s）；中断后续传（已有 6.25 GB）：两端成功。`large_file_passes_keep_the_sender_waiting` 通过；临时把 `OFFLOAD_WHOLE_FILE_BYTES` 改成 `u64::MAX`（即不卸载，不入库）时失败：“the sender must outlast both whole-file passes: IdleTimeout(300ms)”，用时 4.23 s |
| O-N2 的效果 | t13 规模测试，`--test-threads=1` | 改动前的 Linux CI 基线见 N 交接 §9（65,537 文件块阶段 4.6–371.4 s，整项 385.1 s）。只改接收端节拍时，本机 65,537 文件块阶段 1.5–442.5 s，整项 453.2 s，基本没有变快；查明发送端每个文件都整份保存一次记录，是主要开销。两端都改后（`0d4d88f`）：65,537 文件块阶段 11.7–14.1 s，整项 28.2 s；再跑两次，块阶段 4.9–7.6 s 与 4.7–7.4 s，整项 21.9 s 与 21.3 s。20,000 文件块阶段 3.9–4.5 s，整项 8.7 s（改动前 2.1–56.3 s，59.9 s）。这台机器上准备阶段在几次运行之间波动很大（1.1–11.3 s），与改动无关 |
| 复核方本机门槛 | `0d4d88f`；rustc 1.97.0（`rust-toolchain.toml` 钉的 1.99.1 无法下载） | fmt、clippy `-D warnings`、rustdoc `-D warnings` 通过；workspace 测试 68 组结果，1219 passed / 0 failed / 9 ignored；ak-store `sqlite` 52 passed；ak-signal `server-persistence` 115 + 26 + 53 + 26 + 1 passed；Python 366 项 OK；`check_contracts.py` 通过；部署自检、跨 NAT 自检 PASS；Rust 部分用时 544 s。ak-engine 的两项 release 测试（5 GiB、`large_file_passes_keep_the_sender_waiting`）共 35.50 s 通过。首轮 clippy 拦下一处 `bool::then` 的写法，改正后通过。`f9384b9` 单独检查过：fmt、ak-engine 的 clippy 与单元测试（86 passed / 2 ignored）通过 |
| 复核代码提交的 CI | `0d4d88f` push run [44443288135](https://github.com/wmqfl861/AK-sending/actions/runs/44443288135)；手动触发 run [44443287481](https://github.com/wmqfl861/AK-sending/actions/runs/44443287481) | push 运行按并发规则被同时提交的手动运行取消（cancelled）。手动运行 13 项任务全部 success：Linux Rust 任务的 workspace 测试 68 组结果，1219 passed / 0 failed / 9 ignored；`ulimit -n 256` 步骤通过（1.69 s）；Windows Rust 任务 success，其中包括收紧后要求报 `PART_REPLACED` 的替换检测测试；其余任务均 success |
| release 深测（Linux CI） | 同上 run 44443287481 的 release-deep-tests 任务，`0d4d88f` | 全部 ignored 测试通过：ak-engine 的 `large_file_passes_keep_the_sender_waiting` 与 5 GiB 共 30.26 s；`chunk_size_sweep_256mib_loopback` 13.93 s；`bench_netem` 打印 SKIP（runner 不能整形网卡，不算执行）；`single_vs_multistream_512mib_loopback_throughput` 7.91 s；`property_random` 两项 2.03 s；t13 两项共 51.41 s。时间线原文：`T13-20000: phases: manifest received @ 318.0ms, first chunk sent @ 1.3s, last chunk received @ 2.3s, verify phase started @ 2.3s, final ack @ 10.1s, final ack @ 10.1s, done @ 10.1s, done @ 10.1s`；`T13-65537: phases: manifest received @ 1.5s, first chunk sent @ 7.8s, last chunk received @ 11.3s, verify phase started @ 11.4s, final ack @ 34.2s, final ack @ 34.2s, done @ 34.2s, done @ 34.2s`。与 N 交接 §9 的 CI 基线相比，65,537 文件项的块阶段从 366.8 s 降到 3.5 s，整项从 385.1 s 降到 34.2 s；20,000 文件项整项从 56.2 s 降到 10.1 s。只是 Linux 的结论；Windows 归批次 P |
