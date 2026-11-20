# 交接文档：批次 Q（2026-11-19）

> 起点：main 快进到批次 P 复核的最终文档提交（本文随该提交入库）。批次 P 的结论见 [P 复核](REVIEW_2026-11-19-batchP.zh-CN.md)；复核方的命令、运行编号与计数见 [P 交接 §9](HANDOFF-BATCH-P-2026-11-19.zh-CN.md#9-复核方证据2026-11-19)。执行方从本文 §8 证据表开始记录本批的运行编号与计数。
> 每项状态只写在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

## 0. 一句话现状

批次 P 通过：O-N2 在 Windows 上让两个规模测试的块阶段大幅缩短，O-N1 的 5 GiB 项通过。P1 里那项大文件测试的失败是测试设计问题（P-N1），复核方已改正：块阶段只留最后 4 个块，part 文件的前缀在计时之前整段写下并落盘，300 ms 时限不变。改正后的测试由 CI 新增的 Windows 深测任务确认通过。

批次 Q：

- Q0：收尾清理；
- Q1：在本地 Windows 上复跑改正后的大文件测试，与 CI 的结果互相印证；
- Q2（可选）：CR-P2-08 生产代码拆分（原 P4）。

## 1. 复核方在批次 P 分支上的改动

| 项 | 内容 | 主要位置 |
|---|---|---|
| P-N1 | 大文件活动信号测试只在块阶段留 4 个块；part 文件前缀在计时之前整段写下并落盘；测试打印前缀写盘用时与两端阶段时间线，无论成败 | `crates/ak-engine/src/tests.rs`（`large_file_passes_keep_the_sender_waiting`） |
| CI | 新增 `release-deep-tests-windows`：与 Linux 深测一样只在手动触发时运行，先打印临时卷的剩余空间与实时防护状态，再在 windows-latest 上跑 ak-engine 的全部 ignored 测试 | `.github/workflows/ci.yml` |
| 文档 | BACKLOG 的 P3-02 下新增两项：海量小文件的校验阶段、块阶段的检查点保存 | `docs/BACKLOG.zh-CN.md` |

## 2. 接手前必读

### 2.1 大文件测试的输出（P-N1）

`large_file_passes_keep_the_sender_waiting` 先打印一行 `large-file passes: prefix of … bytes written and flushed in …`，结束时再打印 `large-file passes: phases: …` 与两端的结果，无论成败。

- phases 里依次是：接收端收到清单，两端开始续传，两端的第一个和最后一个块，接收端进入校验，两端的 FinalAck 与 Done，发送端返回。
- 失败时凭这一行判断是哪一段超出了 300 ms。
- 准备阶段（收到清单到开始续传）与校验阶段（进入校验到 FinalAck）都应远长于 300 ms：测试要证明的就是这两段期间发送端能等下去。

### 2.2 NTFS 上没有写过的区间

在 NTFS 上，用 `set_len` 延长出来的区间，或只在它后面写过一个字节的区间，都没有真正写过；之后第一次在它后面写入，要等文件系统把整段补零。复核方在 windows-latest 上实测，约 4 GiB 时这次写入要 23–27 s（P 交接 §9）。

- 测试里要造“前一次会话写过的 part 文件”，必须整段真正写下再落盘，不能只用 `set_len`，也不能只写最后一个字节。
- 这样大文件测试在 Windows 上要多占约 4 GiB 磁盘、多花几十秒准备时间；运行前确认磁盘至少剩 10 GB。

### 2.3 手动触发的深测

- 手动触发（workflow_dispatch）时，Linux 与 Windows 两个深测任务都会运行；推送触发的运行里两者都显示 skipped，所以常规运行是 12 项 success、2 项 skipped。
- 手动运行会按并发规则取消同一分支上正在进行的推送运行，记录时两次都要写。

### 2.4 纯移动的证明方法（Q2 会用到）

见 [N 交接 §2.3](HANDOFF-BATCH-N-2026-11-17.zh-CN.md#23-纯移动的证明方法m4n3-会用到)。

### 2.5 注册服务器的就绪行（O-N4）

见 [P 交接 §2.5](HANDOFF-BATCH-P-2026-11-19.zh-CN.md#25-注册服务器的就绪行o-n4)。

## 3. 全项目未完成事项

### 3.1 所有者事项（agent 不得代办）

与 [N 交接 §3.1](HANDOFF-BATCH-N-2026-11-17.zh-CN.md#31-所有者事项agent-不得代办) 相同：E6 生产部署、C4 物理双机跨 NAT、E5 签名、E7 许可证、CR-P3-03、CR-P3-05。批次 M 的 M2 与之后各轮的改动都要等 E6 部署后才进入生产服务器。

### 3.2 工程项

- Q1：本批。
- CR-P2-08：生产代码的拆分（Q2，可选）。
- [BACKLOG](BACKLOG.zh-CN.md) 的 P3-02 之下：
  - 海量小文件的校验阶段：复核方建议的下一项工程，涉及落盘与改名的先后约束，要先定方案；
  - 块阶段的检查点保存；
  - 接收端磁盘中途写满；
  - 发送端的源文件稳定性复核。
- CR-P3-19 的剩余差距、Linux GUI、Android arm64 真机、BACKLOG 的 P3/P4：同 N 交接 §3.2。

## 4. 批次 Q 工作单

| 项 | 步骤 | 验收 |
|---|---|---|
| Q0 | 1) `git fetch --prune`；2) 确认 `git merge-base --is-ancestor origin/dev/batch-p-20261119 origin/main` 成立，然后删除这个远端分支；3) 切到 main 并 `git pull --ff-only`，删除本地旧分支，执行 `git worktree prune`；4) 从 main 切出 `dev/batch-q-<日期>`；5) 在 §8 回填 main 上本文所在提交的 CI 运行编号与结论 | `git branch -a` 只剩 main 和本批分支；`git worktree list` 只剩主目录；§8 有回填行 |
| Q1 | Windows 上先运行 `cargo test --locked --release -p ak-engine -- --ignored --nocapture --test-threads=1`，记录两项的结论与耗时、运行时磁盘的剩余空间，并原样贴上大文件测试打印的 `written and flushed in` 与 `phases:` 两行；再单独连续运行大文件测试 3 次（`cargo test --locked --release -p ak-engine large_file_passes_keep_the_sender_waiting -- --ignored --nocapture`），每次记录退出码与 `phases:` 行 | 全部通过；失败时保留完整输出（两端的结果都在测试输出里），查清根因，不放宽时限 |
| Q2（可选） | CR-P2-08 生产代码拆分，做法同 [N 交接 §4 的 N3 行](HANDOFF-BATCH-N-2026-11-17.zh-CN.md#4-批次-n-工作单)：两步、两个提交，第二步按 §2.4 证明是纯移动 | 测试名单不变；clippy/rustdoc 通过；公开 API 不变 |

并行建议：Q1 要独占 Windows 机器的磁盘；Q2 只改代码结构，可以由另一个执行者并行做，但不要与改接收路径的工作同时进行。

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

补充说明：

- 本机 Rust 应使用 rust-toolchain.toml 钉的 1.99.1，与 CI 一致；改用其他版本时如实记录。
- 深测产生的日志不入库，只在 §8 写次数、结果、耗时和失败时的报错行。
- 缺工具时写“未执行”，不能写成通过。
- 推送前在最终文档上跑一遍 `python scripts/check_repository.py`：批次 P 的第一次推送就是因为 STATUS 缺了一节而在 CI 上失败。

## 6. 文档与分支规则（摘自 AGENTS.md）

- 每项状态只写在状态表里，提交列写真实提交号，不写“本提交”；`check_repository.py` 会检查状态表的每一行，也会拦下被空行隔到表外的行。
- 状态表的“证据摘要”列不写运行编号、计数、倍数和耗时。
- 一项被后续批次确认或推翻时，原地改写它的那一行，不要只在表尾追加新行。改写时保留原有的缺陷描述、修复描述和修复提交号，只补充新的结论。
- run 号和计数只写在本文 §8；STATUS 只写阶段结论、下一步和链接，并链接到本文。改 STATUS 时整份改写，保留历史交接入口，不要只换开头一段。
- 本文不要同时出现“已核”与“尚待 / 待补 / 仍待 / 待执行”（`check_repository.py` 会拦截）。
- 执行方只推 `dev/batch-q-<日期>`；复核通过后由复核方快进合入 main，再由执行方删除分支。任何人都不强推，包括改写自己刚推上去的提交：发现错误就追加一个新提交。
- 新增的 CI 门槛如果失败，先查清原因；不要为了让它通过而削弱它。
- 失败都要记入 §8，不能只记最后一次：
  - CI 运行失败，记运行编号与失败原因；
  - 测试失败后重跑通过，记失败那次的完整输出与原因分析。

## 7. 红线

- 不引入 TURN/relay，服务器不经手文件数据（ADR-001）。
- 不部署、不登录、不修改生产服务器，也不改它的防火墙。
- 不生成签名密钥。
- 配对密钥、令牌、用户文件、真实文件名或路径、完整网络诊断都不入库。
- 模拟或同机测试不写成真机验证。

## 8. 证据表（执行方填写）

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
| main 上批次 P 最终提交的 CI | `2acc3ea` run [44502857824](https://github.com/wmqfl861/AK-sending/actions/runs/44502857824) | completed success（12 项常规任务 success，2 项手动深测 skipped） |
| Q1 全量 release 深测 | 本地 Windows（磁盘 59GB） | **2/2 通过**（263.58s）；`large_file_passes_keep_the_sender_waiting` 通过（P-N1 在 Windows 确认）；`written and flushed in 7.50s`（约 4 GiB 前缀实际写下）；`phases: recv ManifestReceived @ 4.14s, recv resuming 4092 @ 8.62s, send Resuming { chunks: 4092 } @ 8.62s, send chunk #1 sent @ 8.63s, send chunk #4 sent @ 8.64s, recv chunk #1 stored @ 8.67s, recv chunk #4 stored @ 8.72s, recv Verifying @ 8.72s, recv FinalAck { verified: true } @ 13.37s, send FinalAck { verified: true } @ 13.37s, recv Done @ 13.37s, send Done @ 13.37s, send returned ok=true @ 13.37s` |
| Q1 大文件 3 次循环 | 本地 Windows | **3/3 通过**（每次 exit 0）；前缀写盘耗时分别为 9.01s、6.34s、6.73s；phases 行与全量运行一致（P-N1 Windows 确认稳定） |
| Q 全量门槛 | 本地 Windows | fmt/clippy/rustdoc/workspace(1222/0/9)/sqlite/server-persistence/Python/repo/contracts/部署自检/跨NAT自检 全部通过 |
| Q 批次最终 CI | `4ecec40` run [44550426438](https://github.com/wmqfl861/AK-sending/actions/runs/44550426438) | **12/12 常规任务 success**，2 项手动深测 skipped |

## 9. 复核方证据（2026-11-20）

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
| 分支活动 | `dev/batch-q-20261119`，GitHub activity API | 2026-11-19 13:59:50Z 删除 `dev/batch-p-20261119`；15:03:08Z 以 `4ecec40` 创建本批分支（`7b399bf` 与 `4ecec40` 一次推送）；15:16:04Z 普通推送到 `015bb40`。没有强推。远端只剩 main 与本批分支 |
| 本批推送 CI | `4ecec40` run [44550426438](https://github.com/wmqfl861/AK-sending/actions/runs/44550426438)；`015bb40` run [44552857837](https://github.com/wmqfl861/AK-sending/actions/runs/44552857837) | 两次都是 completed success，逐任务 12 项 success、2 项手动深测 skipped；§8 回填的 `2acc3ea` run 44502857824 逐任务同样是 12 项 success、2 项 skipped |
| Windows 工作区测试计数 | run [44552857837](https://github.com/wmqfl861/AK-sending/actions/runs/44552857837) 的 Rust 1.99.1 (windows-latest) 任务日志 | workspace 测试 1222 passed / 0 failed / 9 ignored，与 §8 执行方本机的记录一致；复核方 Linux 工作区是 1220 passed，多出的两项是 Windows 专属测试；ak-store `sqlite` 在 Windows 上 54 passed，Linux 上 52 passed |
| Q1 时间线换算 | §8 全量运行那一行；对照 P 交接 §9 与本表的基线手动运行 | 本机：准备阶段 4.48 s（收到清单 4.14 s 到开始续传 8.62 s），块阶段 4 个块约 0.10 s，校验阶段 4.65 s（进入校验 8.72 s 到 FinalAck 13.37 s），前缀写盘 7.50 s。windows-latest（基线手动运行）：准备 1.13 s，校验 1.16 s，前缀写盘 32.89 s。本机两段约慢四倍，前缀写盘约快四倍，原因没有查 |
| 复核方本机门槛（批次 Q 推送） | `015bb40`；rustc 1.97.0（`rust-toolchain.toml` 钉的 1.99.1 无法下载） | fmt、clippy `-D warnings`、rustdoc `-D warnings` 通过；workspace 测试 68 组结果，1220 passed / 0 failed / 9 ignored；ak-store `sqlite` 52 passed；ak-signal `server-persistence` 115 + 26 + 53 + 26 + 1 passed；Python 366 项 OK；`check_contracts.py` 5/5 PASS；部署自检、跨 NAT 自检 PASS；`check_repository.py` 通过；共 736 s |
| Q-N1 新增检查 | `d912d99` | 在批次 Q 推送的文档（`015bb40`）上报出 50 处：STATUS 缺 45 个入口链接（应链接 55 份），STATUS 正文 2 处通过比，Q1 行 2 处通过比与 1 处耗时区间。在 `2acc3ea` 上只报出 STATUS 缺 9 份更早的文档，状态表与 STATUS 正文没有误报。改正后的文档通过；Python 单元测试 372 项 OK（新增 6 项） |
| Q-N2 基线手动 CI | `3574400`（改动前，含 Windows 深测新增的 t13 步骤）手动触发 run [44685143392](https://github.com/wmqfl861/AK-sending/actions/runs/44685143392) | completed success，14 项任务全部 success。Windows 深测：C 盘剩余约 29.5 GB，实时防护关闭；5 GiB 项与大文件测试通过（前缀写盘 32.89 s，准备 1.13 s，校验 1.16 s）；`T13-20000: phases: manifest received @ 1.2s, first chunk sent @ 11.2s, last chunk received @ 14.7s, verify phase started @ 15.1s, final ack @ 109.5s`，校验阶段 94.4 s；`T13-65537: phases: manifest received @ 3.9s, first chunk sent @ 20.3s, last chunk received @ 32.3s, verify phase started @ 33.4s, final ack @ 423.6s`，校验阶段 390.2 s，约占整项九成，每个文件约 6.0 ms。Linux 深测：20,000 文件项校验 5.4 s（1.4 s 到 6.8 s），65,537 文件项校验 16.0 s（7.0 s 到 23.0 s）。推送运行 44685133811 按并发规则被这次手动运行取消 |
| Q-N2 复核方工作区测量 | 改动前 `3574400`，改动后 `2a31939`；release，各连续两次 | 校验阶段（verify phase started 到 final ack）：65,537 文件项 15.3 s、15.6 s → 7.3 s、7.5 s；20,000 文件项 4.3 s、4.1 s → 2.2 s、2.0 s。改动前每个文件约 0.23 ms，是 Windows 上的二十几分之一 |
| Q-N2 测试与反证 | `2a31939`；复核方工作区 | `verify_failure_waits_for_running_passes_and_a_retry_completes` 在旧的串行循环上通过（只加常量与测试），在新循环上通过；`slow_verify_phase_keeps_the_sender_waiting` 通过。release：5 GiB 项与大文件测试通过，大文件测试校验阶段 1.17 s。临时去掉新循环里的活动信号（不入库）：大文件测试失败，`recv Verifying @ 6.50s, send returned ok=false @ 6.80s`，发送端 `IdleTimeout(300ms)`，接收端 `connection lost` |
| Q-N2 改动后手动 CI | `2a31939` 手动触发 run [44687737210](https://github.com/wmqfl861/AK-sending/actions/runs/44687737210) | completed success，14 项任务全部 success。Windows 深测：实时防护关闭；5 GiB 项与大文件测试通过（前缀写盘 29.77 s，准备 1.65 s，校验 1.73 s）；`T13-20000: phases: manifest received @ 2.1s, first chunk sent @ 14.8s, last chunk received @ 19.6s, verify phase started @ 20.1s, final ack @ 106.3s`，校验阶段 86.2 s（改动前 94.4 s）；`T13-65537: phases: manifest received @ 6.3s, first chunk sent @ 30.6s, last chunk received @ 47.6s, verify phase started @ 49.2s, final ack @ 345.4s`，校验阶段 296.2 s（改动前 390.2 s），每个文件约 4.5 ms。Linux 深测：20,000 文件项校验 2.0 s（改动前 5.4 s），65,537 文件项校验 5.8 s（改动前 16.0 s）；大文件测试校验 1.12 s。推送运行 44687728731 按并发规则被这次手动运行取消 |
| 基准测试第一次手动 CI | `fcc79c3`（Q-N2 的代码加 `bench_commit_cost`）手动触发 run [44689759020](https://github.com/wmqfl861/AK-sending/actions/runs/44689759020) | completed success，14 项任务全部 success；推送运行 44689751031 按并发规则被取消。Windows 深测：实时防护关闭，C 盘剩余约 28.4 GB。基准测试（2,000 个一字节文件，每个文件的平均值，单线程 / 8 线程）：创建写入关闭 218 / 248 µs；打开读取 18 / 6 µs；重新打开已关闭的脏文件落盘 307 / 107 µs；再落盘一次 44 / 14 µs；改时间戳 44 / 61 µs；hard_link 加删除 168 / 185 µs；改名 113 / 181 µs；写穿创建写入关闭 7,473 / 8,307 µs；同一句柄创建写入落盘关闭 4,623 / 5,040 µs。同一任务的 t13：20,000 文件项校验 63.5 s（8.6 s 到 72.1 s），65,537 文件项校验 157.9 s（21.6 s 到 179.5 s），与上一行同一份代码相差近一倍，是 runner 之间的差异。Linux 深测：基准测试落盘 133 / 23 µs，写穿 128 / 44 µs，hard_link 加删除 8 / 11 µs；t13 校验 1.5 s 与 5.6 s |
| 基准测试第二次手动 CI | `29532ee`（基准测试加两项对照）手动触发 run [44691268201](https://github.com/wmqfl861/AK-sending/actions/runs/44691268201) | 第一次尝试 completed failure：13 项 success，Rust 1.99.1 (windows-latest) 的 workspace 测试有一项失败，见下一行；推送运行 44691260075 按并发规则被取消。Windows 深测通过，实时防护关闭，C 盘剩余约 32.4 GB。基准测试的两项对照（每个文件的平均值，单线程 / 8 线程）：按 finish() 现在的顺序逐个文件先落盘、再 hard_link 加删除，3,881 / 3,918 µs；先把全部文件落盘、再逐个 hard_link 加删除，898 / 518 µs。其余各项与第一次同一量级（落盘已关闭的脏文件 243 / 399 µs，hard_link 加删除 395 / 550 µs，同一句柄创建写入落盘 5,427 / 5,112 µs）。同一任务的 t13：20,000 文件项校验 106.5 s（19.8 s 到 126.3 s），65,537 文件项校验 199.7 s（54.8 s 到 254.5 s）。Linux 深测：两项对照 249 / 96 µs 与 228 / 77 µs；t13 校验 2.9 s 与 8.2 s |
| Q-N3 Windows 测试间歇失败 | run [44691268201](https://github.com/wmqfl861/AK-sending/actions/runs/44691268201) 第一次尝试的 Rust 1.99.1 (windows-latest) 任务；同一任务重跑（第二次尝试） | `t03_ipv6_loopback` 的 `probe_round_observes_the_real_advertised_candidates` 失败：`no line containing "the peer wants to send" within 120s`。接收端输出到 `waiting for the peer to join` 为止（先报了 STUN 反射地址、绑定 `[::]` 与 9 个候选地址，打印了任务码）；报错里只有接收端的 stdout 与 stderr，没有发送端与注册服务器的输出，直接原因没有查实。该项在本批其余各次运行里都通过；复核方重跑这一任务，第二次尝试通过，整个运行随之 completed success。`5245ea3` 让这五处等待在超时时一并报出发送端与注册服务器的输出；复核方工作区上 t03 的 12 项通过 |
| 复核方本机门槛（Q-N2） | `2a31939` 的代码；rustc 1.97.0 | fmt、clippy `-D warnings`、rustdoc `-D warnings` 通过；workspace 测试 68 组结果，1221 passed / 0 failed / 9 ignored（新增 1 项）；ak-store `sqlite` 52 passed；ak-signal `server-persistence` 115 + 26 + 53 + 26 + 1 passed；Python 372 项 OK；`check_contracts.py` 5/5 PASS；部署自检、跨 NAT 自检 PASS；`check_repository.py` 通过；共 690 s |
| 复核方本机门槛（最终代码） | `5245ea3` 的代码（加上最终文档）；rustc 1.97.0 | fmt、clippy `-D warnings`、rustdoc `-D warnings` 通过；workspace 测试 69 组结果，1221 passed / 0 failed / 10 ignored（新增基准测试一组、一项 ignored）；ak-store `sqlite` 52 passed；ak-signal `server-persistence` 115 + 26 + 53 + 26 + 1 passed；Python 372 项 OK；`check_contracts.py` 5/5 PASS；部署自检、跨 NAT 自检 PASS；`check_repository.py` 通过；共 654 s。基准测试在复核方工作区的两项对照（单线程 / 8 线程）：逐个文件 227 / 84 µs，先全部落盘 159 / 68 µs，与 Linux 深测同一量级 |
