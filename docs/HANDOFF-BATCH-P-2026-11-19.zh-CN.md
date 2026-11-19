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
