# 交接文档：批次 P（2026-11-19）

> 起点：main 快进到批次 O 复核的最终文档提交（本文随该提交入库）。批次 O 的结论见 [O 复核](REVIEW_2026-11-19-batchO.zh-CN.md)；复核方的命令、运行编号与计数见 [O 交接 §9](HANDOFF-BATCH-O-2026-11-18.zh-CN.md#9-复核方证据2026-11-19)。执行方从本文 §8 证据表开始记录本批的运行编号与计数。
> 每项状态只写在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

## 0. 一句话现状

批次 O 通过：Windows 上的确认全部通过，65,537 文件项第一次在 Windows 上通过。复核方据 O2 的 Windows 数据又修了两处，都只在 Linux 上验证过：

- O-N1（P1）：单个大文件的整文件处理改到阻塞线程池上执行，期间照常发活动信号；
- O-N2（P2）：块阶段两端的检查点至多每秒写盘一次。

第一次合入 main 后，复核方又修了一处既有缺陷（O-N4，P2）：注册服务器改为先监听、再打印就绪行，绑定端口或打开数据库失败时直接报错退出；起注册服务器的集成测试改为从就绪行读端口。它由 main 的一次 Windows CI 失败引出；修复后 Windows CI 通过，本批不另设确认项。

批次 P 在本地 Windows 上确认 O-N1、O-N2 两处改动：

- P0：收尾清理；
- P1：大文件 release 测试；
- P2：规模测试，测量 O-N2 的效果；
- P3：计时类测试循环；
- P4：可选的拆分工程项。

## 1. 复核方在批次 O 分支上的改动

| 项 | 内容 | 主要位置 |
|---|---|---|
| O-N1 | 不小于 64 MiB 的文件，其整文件处理（准备阶段的续传前缀与已提交文件重新哈希、校验阶段的整文件摘要与落盘）放到 tokio 阻塞线程池执行，接收任务照常按间隔发活动信号；ak-storage 的替换检测测试在 Windows 上收紧为必须报 `PART_REPLACED` | `crates/ak-engine/src/lib.rs`（`offload_with_liveness`、`OFFLOAD_WHOLE_FILE_BYTES`、`receive_frozen_body`）、`crates/ak-engine/src/tests.rs`、`crates/ak-storage/src/lib.rs` |
| O-N2 | 块阶段两端的检查点都至多每秒写盘一次（发送端原来每个文件写一次，是主要开销；接收端原来每 16 个块写一次）；进度节奏不变 | `crates/ak-engine/src/lib.rs`（`ChunkCheckpointCadence`、`CHUNK_CHECKPOINT_INTERVAL`） |
| O-N4 | `registry-server` 先绑定端口、再打开数据库，都成功后才打印就绪行（打印实际绑定的地址，`--port 0` 可用），任一步失败或服务循环自行结束时命令带错误退出；五个起注册服务器的集成测试文件改用 `--port 0` 并从就绪行读端口，等待超时的报错带上子进程的 stderr | `crates/ak-cli/src/main.rs`（`registry_server_command`）、`crates/ak-signal/src/server_v2/engine.rs`（`bind_v2`）、`crates/ak-itests/tests/` 下 t03_ipv6_loopback、send_admission_cli、invite_pairing_security、attack_recovery、invite_pairing_cli_e2e 五个文件 |
| 文档 | PROTOCOL 的接收端活动信号与持久边界、engine 模块文档、BACKLOG、README 的 registry-server 说明 | `docs/PROTOCOL.zh-CN.md`、`docs/BACKLOG.zh-CN.md`、`README.md` |

## 2. 接手前必读

### 2.1 大文件的活动信号（O-N1）

- 发送端等每一帧都受空闲时限约束（默认 30 秒）。接收端在准备阶段和校验阶段不发常规帧，所以要靠活动信号：准备阶段是空的 `more = true` ResumeRequest 分片，校验阶段是发给带 `CAP_PAUSE_RESUME` 的发送端的无操作 `Control::Resume`。
- 小文件在文件之间检查间隔；不小于 64 MiB 的文件，其整文件处理在阻塞线程池上跑，接收任务按间隔继续发信号。改接收路径时，不要把任何可能耗时数秒的整文件处理放回接收任务里同步执行。
- 验证这类行为的测试要用多线程运行时：单线程测试运行时里，接收端阻塞时发送端的计时器也停了，会掩盖问题。

### 2.2 块阶段检查点（O-N2）

- 检查点每次都整份重写，大小与文件数成正比；现在块阶段两端都至多每秒写一次，接收端在校验阶段开始前再完整写一次。
- 续传不依赖这些中途保存：接收端重新哈希 part 文件，发送端按接收端的 ResumeRequest 续传，所以写盘变少不影响续传的正确性。
- `BytesAcked` 进度仍在每第 16 个块报告一次，节奏不变。

### 2.3 规模测试的阶段时间线

`t13_many_files` 的规模测试输出一行 `T13-…: phases: …`，含义见 [O 交接 §2.3](HANDOFF-BATCH-O-2026-11-18.zh-CN.md#23-规模测试的阶段时间线)。O2 的两行 Windows 原文就是 P2 的对照基线。

### 2.4 纯移动的证明方法（P4 会用到）

见 [N 交接 §2.3](HANDOFF-BATCH-N-2026-11-17.zh-CN.md#23-纯移动的证明方法m4n3-会用到)。

### 2.5 注册服务器的就绪行（O-N4）

- `registry-server` 的两行就绪输出（`v2 pairing registry on tls <地址> …` 与 `registry id …`）现在表示它已经在监听；端口被占或数据库打不开时，它不打印这两行，直接报错退出。
- 新写的测试或脚本起注册服务器时，用 `--port 0`，从 `v2 pairing registry on tls` 那一行读出实际端口；不要先探一个空闲端口再交给子进程绑定，两步之间端口可能被别的连接占用。
- 等子进程输出超时的报错，要带上该进程的 stderr。

## 3. 全项目未完成事项

### 3.1 所有者事项（agent 不得代办）

与 [N 交接 §3.1](HANDOFF-BATCH-N-2026-11-17.zh-CN.md#31-所有者事项agent-不得代办) 相同：E6 生产部署、C4 物理双机跨 NAT、E5 签名、E7 许可证、CR-P3-03、CR-P3-05。批次 M 的 M2 与之后各轮的改动都要等 E6 部署后才进入生产服务器。

### 3.2 工程项

- P1–P3：本批。
- CR-P2-08：生产代码的拆分（P4，可选）。
- 复核登记的后续工程项（接收端磁盘中途写满时发送端只看到 connection lost；发送端在 Finalize 前逐个核对源文件，期间不发帧）记在 [BACKLOG](BACKLOG.zh-CN.md) 的 P3-02 之下。
- CR-P3-19 的剩余差距、Linux GUI、Android arm64 真机、BACKLOG 的 P3/P4：同 N 交接 §3.2。

## 4. 批次 P 工作单

| 项 | 步骤 | 验收 |
|---|---|---|
| P0 | 1) `git fetch --prune`；2) 确认 `git merge-base --is-ancestor origin/dev/batch-o-20261118 origin/main` 成立，然后删除这个远端分支；3) 切到 main 并 `git pull --ff-only`，删除本地旧分支，执行 `git worktree prune`；4) 从 main 切出 `dev/batch-p-<日期>`；5) 在 §8 回填 main 上本文所在提交的 CI 运行编号与结论 | `git branch -a` 只剩 main 和本批分支；`git worktree list` 只剩主目录；§8 有回填行 |
| P1 | Windows 上运行 `cargo test --locked --release -p ak-engine -- --ignored --nocapture --test-threads=1`，逐项记录 `five_gib_sparse_file_transfers_with_u64_offsets` 与 `large_file_passes_keep_the_sender_waiting` 的结论与耗时，并记下运行时磁盘的剩余空间 | 两项都通过；失败时保留完整输出（两端的报错都要），查清根因，不放宽超时 |
| P2 | Windows 上运行 `cargo test --locked --release -p ak-itests --test t13_many_files -- --ignored --nocapture --test-threads=1`，记录两个规模测试的结论、耗时，并原样贴上 `phases:` 那一行；把块阶段（first chunk sent 到 last chunk received）与 O2 的基线对照 | 两项都通过；§8 写明块阶段相对 O2 的变化。没有变快也如实记录，不要为此改代码 |
| P3 | Windows 上用 PowerShell 循环各连续运行 10 次 `cargo test --locked -p ak-engine --lib offloaded_pass_keeps_signalling_until_it_finishes` 与 `cargo test --locked -p ak-engine --lib chunk_checkpoint_saves_at_most_once_per_interval`，记录每次退出码 | 全部通过；失败时保留完整输出、查清根因，不放宽任何计时或断言 |
| P4（可选） | CR-P2-08 生产代码拆分，做法同 [N 交接 §4 的 N3 行](HANDOFF-BATCH-N-2026-11-17.zh-CN.md#4-批次-n-工作单)：两步、两个提交，第二步按 §2.4 证明是纯移动 | 测试名单不变；clippy/rustdoc 通过；公开 API 不变 |

并行建议：P1 与 P2 都要独占 Windows 机器的 CPU 与磁盘，串行做；P3 可以放在 P1 之前。P4 只改代码结构，可以由另一个执行者并行做，但不要与改接收路径的工作同时进行。

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

P3 的 PowerShell 循环示例：

```powershell
$tests = 'offloaded_pass_keeps_signalling_until_it_finishes', 'chunk_checkpoint_saves_at_most_once_per_interval'
$results = foreach ($t in $tests) {
  1..10 | ForEach-Object {
    cargo test --locked -p ak-engine --lib $t *> "p3-$t-$_.log"
    [pscustomobject]@{ Test = $t; Run = $_; ExitCode = $LASTEXITCODE }
  }
}
$results | Format-Table
```

补充说明：

- 本机 Rust 应使用 rust-toolchain.toml 钉的 1.99.1，与 CI 一致；改用其他版本时如实记录。
- 循环与深测产生的日志不入库，只在 §8 写次数、结果、耗时和失败时的报错行。
- 缺工具时写“未执行”，不能写成通过。

## 6. 文档与分支规则（摘自 AGENTS.md）

- 每项状态只写在状态表里，提交列写真实提交号，不写“本提交”；`check_repository.py` 会检查状态表的每一行，也会拦下被空行隔到表外的行。状态表的“证据摘要”列不写运行编号、计数和耗时。
- 一项被后续批次确认或推翻时，原地改写它的那一行，不要只在表尾追加新行。
- run 号和计数只写在本文 §8；STATUS 只写阶段结论、下一步和链接，并链接到本文。改 STATUS 时整份改写，不要只换开头一段。
- 本文不要同时出现“已核”与“尚待 / 待补 / 仍待 / 待执行”（`check_repository.py` 会拦截）。
- 执行方只推 `dev/batch-p-<日期>`；复核通过后由复核方快进合入 main，再由执行方删除分支。任何人都不强推，包括改写自己刚推上去的提交：发现错误就追加一个新提交。
- 新增的 CI 门槛如果失败，先查清原因；不要为了让它通过而削弱它。
- 测试失败后重跑通过时，失败那次的完整输出与原因分析也要记入 §8，不能只记最后一次。

## 7. 红线

- 不引入 TURN/relay，服务器不经手文件数据（ADR-001）。
- 不部署、不登录、不修改生产服务器，也不改它的防火墙。
- 不生成签名密钥。
- 配对密钥、令牌、用户文件、真实文件名或路径、完整网络诊断都不入库。
- 模拟或同机测试不写成真机验证。

## 8. 证据表（执行方填写）

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
| main 上批次 O 最终提交的 CI | `7bb66a2` run [44452952190](https://github.com/wmqfl861/AK-sending/actions/runs/44452952190) | completed success（12 项常规任务 success，1 项手动深测 skipped） |
| P3 offloaded_pass 10 次 | 本地 Windows | **10/10 通过，0 失败** |
| P3 chunk_checkpoint 10 次 | 本地 Windows | **10/10 通过，0 失败** |
| P1 five_gib_sparse | 本地 Windows | **通过**（运行时磁盘 19GB）；上一批两次磁盘不足导致的失败见 O 交接 §8 |
| P1 large_file_passes | 本地 Windows | **失败**（确定性）：`the sender must outlast both whole-file passes: IdleTimeout(300ms)`（tests.rs:2231）；300ms 极紧预算下活动信号在 Windows 上不够快——复核方 O-N1 实现的 Windows 特有发现，需复核方处理 |
| P2 20,000 文件 | 本地 Windows | **通过**；`T13-20000: phases: manifest received @ 9.3s, first chunk sent @ 28.9s, last chunk received @ 43.9s, verify phase started @ 46.8s, final ack @ 148.8s, final ack @ 148.8s, done @ 148.9s, done @ 148.9s`；块阶段（28.9→43.9 = **15.0s**，O2 基线 277.2s → **~18x 快**） |
| P2 65,537 文件 | 本地 Windows | **通过**；`T13-65537: phases: manifest received @ 28.7s, first chunk sent @ 72.0s, last chunk received @ 117.2s, verify phase started @ 126.1s, final ack @ 467.8s, final ack @ 467.8s, done @ 467.8s, done @ 467.9s`；块阶段（72.0→117.2 = **45.2s**，O2 基线 2075.4s → **~46x 快**） |
| P 全量门槛 | 本地 Windows | fmt/clippy/rustdoc/workspace(1222/0/9)/sqlite/server-persistence/Python(366/366，首次 errors=2 不复现)/repo/contracts/部署自检/跨NAT自检 全部通过 |
| P 批次最终 CI | `57c0a15` run [44476483911](https://github.com/wmqfl861/AK-sending/actions/runs/44476483911) | **12/12 常规任务 success**，1 项手动深测 skipped |

## 9. 复核方证据（2026-11-19）

复核方是 Claude，在云端 Linux 工作区（2 vCPU，8 GB 内存，ext4，virtio 磁盘）操作，没有接触生产服务器，也没有本地 Windows 环境；Windows 上的复现与确认借助 CI 的 windows-latest runner。结论见 [P 复核](REVIEW_2026-11-19-batchP.zh-CN.md)，逐项状态只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。“探针”指不入库的一次性改动；除注明外都是 release 构建。

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
| 远端分支与推送记录 | GitHub API（activity），2026-11-19 | `dev/batch-o-20261118`：04:21:38Z `branch_deletion`（`7bb66a2`）。`dev/batch-p-20261119`：07:01:11Z `branch_creation` → `cd39efe`；07:01:49Z `push` `cd39efe` → `57c0a15`；07:14:40Z `push` `57c0a15` → `b88dc04`；没有强推。`f6b54b0` 没有单独推送过 |
| §8 引用的运行与未记录的运行 | run [44452952190](https://github.com/wmqfl861/AK-sending/actions/runs/44452952190)、[44476483911](https://github.com/wmqfl861/AK-sending/actions/runs/44476483911)、[44476405318](https://github.com/wmqfl861/AK-sending/actions/runs/44476405318)、[44478114212](https://github.com/wmqfl861/AK-sending/actions/runs/44478114212) | `7bb66a2`（main）与 `57c0a15` 的 push 运行都是 completed success（12 项 success，深测 skipped），与 §8 一致。`cd39efe` 的 push 运行：Repository checks failure，报 `docs/STATUS.zh-CN.md: '最新验证' section not found`，其余任务在 `57c0a15` 推送后被取消（cancelled 9、success 2、skipped 1）；§8 没有记录这一次。`b88dc04` 的 push 运行 completed success（12 项 success，深测 skipped） |
| P2 的阶段换算 | 执行方数据（本文 §8 与 O 交接 §8），复核方计算 | 20,000 文件：准备 19.6 s（O2 17.5 s），块阶段 15.0 s（277.2 s），核对源文件 2.9 s（3.0 s），校验 102.0 s（62.4 s），整项 148.9 s（369.2 s）。65,537 文件：准备 43.3 s（41.6 s），块阶段 45.2 s（2,075.4 s），核对源文件 8.9 s（8.7 s），校验 341.7 s（214.3 s），整项 467.9 s（2,366.3 s），校验占整项的 73%。Linux CI 上 O-N2 前后（N 交接 §9 → O 交接 §9）的同一走向：65,537 文件校验 13.5 s → 22.8 s，20,000 文件 5.6 s → 7.8 s，块阶段从数分钟降到几秒 |
| P-N1：Linux 上的间歇失败 | `b88dc04` 的代码加 `fcc6c3e` 的时间线，复核方工作区 | 3 次里 2 次失败：发送端 `IdleTimeout(300ms)`，时间线停在块阶段（接收端存下第 1 块之后约 1.7–2.2 s，第 1024 块之前），接收端报 `connection lost` 或 `PeerClosed`。探针记录块阶段每次写块与检查点保存的用时：6 次运行都通过，每次都有一次检查点保存超过 100 ms（131–232 ms），写块没有一次超过 100 ms。只用 strace 跟踪 fsync（开销很小）时 4 次都通过，块阶段检查点保存的 fsync 每次 48–160 ms |
| P-N1：Windows 上的失败（CI 复现） | `fcc6c3e`，手动触发 run [44494711082](https://github.com/wmqfl861/AK-sending/actions/runs/44494711082) 的 Windows 深测任务 | windows-latest，C 盘剩余约 31.5 GB。5 GiB 项通过。大文件测试失败，时间线：`recv ManifestReceived @ 3.23s, recv resuming 3072 @ 4.82s, send Resuming { chunks: 3072 } @ 4.83s, send chunk #1 sent @ 4.83s, send returned ok=false @ 5.14s`；发送端 `IdleTimeout(300ms)`，接收端 `connection lost`，第 1 块始终没有存下。同一运行的 Linux 深测任务里这一项通过（块阶段 2.75 s）。该运行其余 12 项任务 success |
| P-N1 第一次改正（只写前缀末字节） | `14137c1`：复核方工作区；手动触发 run [44496509492](https://github.com/wmqfl861/AK-sending/actions/runs/44496509492) | 本机连续 8 次通过，每次 6.1–8.8 s，写末字节用时 0.5–1.1 ms（稀疏文件）；时间线示例：准备 2.58 s，块阶段约 10 ms，校验 1.16 s。探针去掉卸载（`OFFLOAD_WHOLE_FILE_BYTES = u64::MAX`）后失败：`recv ManifestReceived @ 4.91s, send returned ok=false @ 5.21s`。CI：Linux 深测通过；Windows 深测仍失败，写末字节用时 5.80 ms，时间线 `recv ManifestReceived @ 2.27s, recv resuming 4092 @ 3.87s, send Resuming { chunks: 4092 } @ 3.87s, send chunk #1 sent @ 3.87s, send chunk #4 sent @ 3.88s, send returned ok=false @ 4.18s`，接收端 `connection lost`。该运行的 Windows Rust 任务被下一次推送取消，其余 12 项 success |
| Windows 探针（临时测试，随后删去） | `acf4beb`，手动触发 run [44497916060](https://github.com/wmqfl861/AK-sending/actions/runs/44497916060) 的 Windows 深测任务 | windows-latest：`AntivirusEnabled True`、`RealTimeProtectionEnabled False`，C 盘剩余约 31.5 GB。约 4 GiB 的前缀，先整读一遍、再以读写方式重开，然后在前缀末尾写 1 MiB：`set_len` 造出的前缀，第一次写入 23.07 s；只写末字节（用时 2.29 ms），第一次写入 26.42 s；只写末字节并等 10 s，第一次写入 27.21 s；整段写下（用时 25.40 s），第一次写入 0.61 ms。四种情况的第二次写入都不到 1 ms。同一任务里大文件测试仍失败（写末字节 2.22 ms，时间线停在发送第 4 块之后 300 ms）；Linux 深测通过；该运行的 Windows Rust 任务被下一次推送取消，其余 12 项 success。Linux 上同一探针四种情况的第一次写入都不到 1 ms |
| P-N1 最终改正（整段写下前缀） | `f8c976b`，复核方工作区 | 连续 5 次通过，每次 11.0–12.5 s，写下并落盘前缀用时 3.4–5.4 s。时间线示例：准备 1.49 s，块阶段约 10 ms，校验 1.19 s。探针去掉卸载后失败：`recv ManifestReceived @ 3.21s, send returned ok=false @ 3.51s` |
| P-N1 最终改正的 CI | `f8c976b` 手动触发 run [44499375347](https://github.com/wmqfl861/AK-sending/actions/runs/44499375347) | completed success，14 项任务全部 success。Windows 深测：C 盘剩余约 29.5 GB，实时防护关闭；5 GiB 项与大文件测试都通过（两项共 90.03 s），写下并落盘前缀 30.40 s，时间线：准备 1.15 s，块阶段约 20 ms，校验 1.17 s。Linux 深测全部通过，写下并落盘前缀 40.17 s，准备 13.08 s，校验 0.57 s。四个复核提交的推送运行（44494712613、44496508745、44497917312、44499375227）都按并发规则被随后的手动运行取消 |
| 复核方本机门槛 | `14137c1`；rustc 1.97.0（`rust-toolchain.toml` 钉的 1.99.1 无法下载） | fmt、clippy `-D warnings`、rustdoc `-D warnings` 通过；workspace 测试 68 组结果，1220 passed / 0 failed / 9 ignored；ak-store `sqlite` 52 passed；ak-signal `server-persistence` 115 + 26 + 53 + 26 + 1 passed；Rust 部分用时 599 s。`f8c976b` 只改了这项 ignored 测试的准备步骤与 CI 注释，并删去探针，在它上面重跑了 fmt、ak-engine 的 clippy 与这项 release 测试（见上面两行）。Python 366 项 OK；`check_contracts.py` 5/5 PASS；部署自检、跨 NAT 自检 PASS；`check_repository.py` 在最终文档上通过 |
