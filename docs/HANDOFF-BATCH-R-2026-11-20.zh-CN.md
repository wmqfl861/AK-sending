# 交接文档：批次 R（2026-11-20）

> 起点：main 快进到批次 Q 复核的最终文档提交（本文随该提交入库）。批次 Q 的结论见 [Q 复核](REVIEW_2026-11-20-batchQ.zh-CN.md)；复核方的命令、运行编号与计数见 [Q 交接 §9](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md#9-复核方证据2026-11-20)。执行方从本文 §8 证据表开始记录本批的运行编号与计数。
> 每项状态只写在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

## 0. 一句话现状

批次 Q 通过：P-N1 改正后的大文件测试在本地 Windows 上通过。记录问题与批次 O、P 同类（Q-N1），复核方已更正，并在 `scripts/check_repository.py` 里加了两项检查，以后这类问题会在推送前的本机检查和 CI 上直接报错。

复核方还做了 BACKLOG 推荐的下一项工程（Q-N2，P2）：接收端的校验阶段同时处理至多 8 个文件，每个文件仍是整文件摘要、落盘之后才改名。Linux 上两个规模测试的校验阶段明显缩短；Windows CI 上效果小，而且 windows-latest 每次分到的机器性能不同，几次运行分不出改动的效果。复核方新加的基准测试 `bench_commit_cost` 显示，Windows 上逐个文件交替“落盘、建立最终名”时每次落盘都很贵，先把一批文件落盘、再建立最终名便宜几倍；复核方准备据此做下一步，需要先在本地 Windows 上复测。

批次 R 在本地 Windows 上确认 Q-N2 并复测基准：

- R0：收尾清理；
- R1：规模测试，与 P2 在同一台机器上对照，测量 Q-N2 的效果；
- R2：ak-engine 的 release 深测，大文件那条路径也换成了新循环；
- R3：基准测试 `bench_commit_cost`。

## 1. 复核方在批次 Q 分支上的改动

| 项 | 内容 | 主要位置 |
|---|---|---|
| Q-N1 | 两项新的仓库检查：STATUS 必须链接 docs 下全部交接与复核文档以及项目入口文档；状态表批次 L 以后各行的“状态”“证据摘要”两列和 STATUS“最新验证”以外的部分不得出现运行编号、通过比、测试计数、倍数和带小数或区间的耗时 | `scripts/check_repository.py`、`scripts/tests/test_repository_checks.py` |
| Q-N2 | 校验阶段每个文件的 finish()（整文件摘要、落盘、建立最终名）都放到 tokio 阻塞线程池上执行，同时至多 `VERIFY_PARALLEL_FILES` = 8 个，不小于 64 MiB 的文件同一时间至多一个；接收任务等待期间照常发活动信号，进度按完成的先后报告；某个文件失败后不再开始新的文件，已开始的全部返回之后才报错 | `crates/ak-engine/src/lib.rs`（`VERIFY_PARALLEL_FILES` 与校验循环）、`crates/ak-engine/src/tests.rs`（`verify_failure_waits_for_running_passes_and_a_retry_completes`） |
| 基准测试 | `bench_commit_cost`（ignored，release）：在 2,000 个一字节文件上计时校验阶段的各步（创建写入、读、落盘、写穿、改时间戳、hard_link 加删除、改名），以及“逐个文件落盘再建立最终名”与“先全部落盘、再逐个建立最终名”两种顺序；单线程与 8 线程各一遍，只打印平均值，不打印路径 | `crates/ak-itests/tests/bench_commit_cost.rs` |
| Q-N3 | t03 里等待接收端“the peer wants to send”的五处，超时时一并报出发送端与注册服务器的输出（含 stderr） | `crates/ak-itests/tests/t03_ipv6_loopback.rs`（`LineLog::wait_for_reporting`） |
| CI | 只在手动触发时运行的 Windows 深测任务新增 t13 的两个规模测试与 `bench_commit_cost` | `.github/workflows/ci.yml`（`release-deep-tests-windows`） |
| 文档 | PROTOCOL 的接收端活动信号一段、engine 模块文档、BACKLOG | `docs/PROTOCOL.zh-CN.md`、`docs/BACKLOG.zh-CN.md` |

## 2. 接手前必读

### 2.1 校验阶段的并行（Q-N2）

- 每个文件内部的顺序没有变：整文件摘要通过、`sync_all` 落盘之后才建立最终文件名。改接收路径时，这一顺序不能放松。
- 进度事件 `FileVerified` / `FileCommitted` 现在按完成的先后到达，不再按清单顺序；界面和 CLI 都按文件序号处理，不受影响。
- 某个文件失败时，接收端等已开始的文件全部返回才报错；报错之后目标目录里不会再有写入。
- 规模测试 `T13-…: phases:` 行里，校验阶段是 `verify phase started` 到 `final ack`。P2 的两行 Windows 原文（[P 交接 §8](HANDOFF-BATCH-P-2026-11-19.zh-CN.md#8-证据表执行方填写)）是 R1 的对照基线，复核方换算出的校验阶段在 [P 交接 §9](HANDOFF-BATCH-P-2026-11-19.zh-CN.md#9-复核方证据2026-11-19)。
- CI 的 Windows 深测上，同一份代码两次运行的校验阶段可以相差近一倍，所以 Q-N2 在 Windows 上的效果要看 R1 在同一台机器上与 P2 的对照。没有明显变快也如实记录。

### 2.2 每文件落盘开销的基准测试

- `bench_commit_cost` 每一行形如 `commit-cost: <步骤> threads=<n>: <总耗时> total, <每个文件> per file`，单线程与 8 线程各一组，最后两组是两种顺序的对照：`per file: sync_all, then hard_link+remove_file` 与 `all sync_all, then all hard_link+remove_file`。
- windows-latest 上前者每个文件约 3.9 ms，后者约 0.9 ms（8 线程约 0.5 ms），数字见 [Q 交接 §9](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md#9-复核方证据2026-11-20)。R3 要看本地机器上是否同样如此；实时防护开着时结果可能不同，所以先记下它的状态。
- 它只计时文件系统操作，不测 AK-sending 的代码，也不打印路径。

### 2.3 新增的仓库检查（Q-N1）

- `python scripts/check_repository.py` 现在会拦下：STATUS 少链接了 docs 下任何一份交接或复核文档，或 AGENTS、CHANGELOG、ROADMAP、BACKLOG、OPERATIONS、SUPPORT_MATRIX；状态表批次行（编号形如 R0、R1、R-N1）的“状态”“证据摘要”两列里出现运行编号、`2/2` 这样的通过比、`passed`/`failed` 计数、`18x` 或“3 倍”、`7.50s` 或 `7-9s` 这样的耗时；STATUS“最新验证”以外的部分出现同类数字。
- 这些数字只写在本文 §8。整数的时间（如“300 ms 时限”）不拦，但实测耗时也不要写进状态表。
- 改 STATUS 时整份改写，保留“交接入口”里的全部链接，只在最前面加上新的交接与复核。

### 2.4 手动触发的深测

- 手动触发（workflow_dispatch）时，Linux 与 Windows 两个深测任务都会运行，Windows 深测现在也跑 t13 的两个规模测试与 `bench_commit_cost`；推送触发的运行里两者都显示 skipped，常规运行是 12 项 success、2 项 skipped。
- 手动运行会按并发规则取消同一分支上正在进行的推送运行，记录时两次都要写。

### 2.5 NTFS 上没有写过的区间

见 [Q 交接 §2.2](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md#22-ntfs-上没有写过的区间)。大文件测试运行前确认磁盘至少剩 10 GB。

### 2.6 间歇失败的测试（Q-N3）

- 批次 Q 期间，CI 的 Windows Rust 任务里 `t03_ipv6_loopback` 的 `probe_round_observes_the_real_advertised_candidates` 失败过一次，重跑通过，直接原因没有查实。现在这类等待超时会一并报出发送端与注册服务器的输出。
- 门槛里的测试在本地失败时，把完整报错（含 `--- sender, output so far:` 等段落）贴进 §8，再重跑；不要只记重跑通过。

## 3. 全项目未完成事项

### 3.1 所有者事项（agent 不得代办）

与 [N 交接 §3.1](HANDOFF-BATCH-N-2026-11-17.zh-CN.md#31-所有者事项agent-不得代办) 相同：E6 生产部署、C4 物理双机跨 NAT、E5 签名、E7 许可证、CR-P3-03、CR-P3-05。批次 M 的 M2 与之后各轮的改动都要等 E6 部署后才进入生产服务器。

### 3.2 工程项

- R1、R2：本批。
- CR-P2-08：生产代码的拆分。不再列给执行方；排在分批落盘之后，由复核方安排。
- [BACKLOG](BACKLOG.zh-CN.md) 的 P3-02 之下：
  - 海量小文件的校验阶段：Q-N2 是第一步；下一步是分批落盘，finish() 拆成“摘要并落盘”与“建立最终名”两步，复核方在 R3 确认之后做；
  - 块阶段的检查点保存；
  - 接收端磁盘中途写满；
  - 发送端的源文件稳定性复核。
- CR-P3-19 的剩余差距、Linux GUI、Android arm64 真机、BACKLOG 的 P3/P4：同 N 交接 §3.2。

## 4. 批次 R 工作单

| 项 | 步骤 | 验收 |
|---|---|---|
| R0 | 1) `git fetch --prune`；2) 确认 `git merge-base --is-ancestor origin/dev/batch-q-20261119 origin/main` 成立，然后删除这个远端分支；3) 切到 main 并 `git pull --ff-only`，删除本地旧分支，执行 `git worktree prune`；4) 从 main 切出 `dev/batch-r-<日期>`；5) 在 §8 回填 main 上本文所在提交的 CI 运行编号与结论 | `git branch -a` 只剩 main 和本批分支；`git worktree list` 只剩主目录；两条命令的输出原样贴进 §8；§8 有回填行 |
| R1 | Windows 上先运行 PowerShell 的 `Get-MpComputerStatus \| Format-List AntivirusEnabled, RealTimeProtectionEnabled` 并记下结果，再连续运行两次 `cargo test --locked --release -p ak-itests --test t13_many_files -- --ignored --nocapture --test-threads=1`；每次记录两个规模测试的结论、耗时，并原样贴上两行 `phases:`；算出校验阶段（verify phase started 到 final ack），与 P2 的基线对照 | 四项都通过；§8 逐次写明校验阶段相对 P2 的变化。没有变快也如实记录，不要为此改代码 |
| R2 | Windows 上运行 `cargo test --locked --release -p ak-engine -- --ignored --nocapture --test-threads=1`，记录两项的结论与耗时、运行时磁盘的剩余空间，并原样贴上大文件测试打印的 `written and flushed in` 与 `phases:` 两行 | 两项都通过；失败时保留完整输出（两端的结果都在测试输出里），查清根因，不放宽时限 |
| R3 | Windows 上连续运行两次 `cargo test --locked --release -p ak-itests --test bench_commit_cost -- --ignored --nocapture --test-threads=1`，每次原样贴出全部 `commit-cost:` 行；实时防护的状态用 R1 记下的那一次 | 两次都运行完成；§8 写明本机上两种顺序的对照（每个文件的耗时）与 windows-latest 是否同一方向。这是测量，不要为此改代码 |

并行建议：R1、R2、R3 都要独占 Windows 机器的 CPU 与磁盘，串行做。

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
- 推送前在最终文档上跑一遍 `python scripts/check_repository.py`；它现在也检查 STATUS 的入口链接和状态表、STATUS 里的数字（§2.3）。

## 6. 文档与分支规则（摘自 AGENTS.md）

- 每项状态只写在状态表里，提交列写真实提交号，不写“本提交”；`check_repository.py` 会检查状态表的每一行，也会拦下被空行隔到表外的行。
- 状态表的“证据摘要”列不写运行编号、计数、倍数和耗时（§2.3 的检查会拦下大部分）。
- 一项被后续批次确认或推翻时，原地改写它的那一行，不要只在表尾追加新行。改写时保留原有的缺陷描述、修复描述和修复提交号，只补充新的结论。本批确认的是 Q-N2 行。
- run 号和计数只写在本文 §8；STATUS 只写阶段结论、下一步和链接，并链接到本文。改 STATUS 时整份改写，保留历史交接入口，不要只换开头一段。
- 本文不要同时出现“已核”与“尚待 / 待补 / 仍待 / 待执行”（`check_repository.py` 会拦截）。
- 执行方只推 `dev/batch-r-<日期>`；复核通过后由复核方快进合入 main，再由执行方删除分支。任何人都不强推，包括改写自己刚推上去的提交：发现错误就追加一个新提交。
- 新增的 CI 门槛如果失败，先查清原因；不要为了让它通过而削弱它。
- 失败都要记入 §8，不能只记最后一次：
  - CI 运行失败，记运行编号与失败原因；
  - 测试失败后重跑通过，记失败那次的完整输出与原因分析。
- 验收要求原样贴出的输出（命令结果、`phases:` 行），每次都要贴，不能写成“与上一次一致”。
- 没有做的项，在 §8 写明没有做和原因。

## 7. 红线

- 不引入 TURN/relay，服务器不经手文件数据（ADR-001）。
- 不部署、不登录、不修改生产服务器，也不改它的防火墙。
- 不生成签名密钥。
- 配对密钥、令牌、用户文件、真实文件名或路径、完整网络诊断都不入库。
- 模拟或同机测试不写成真机验证。

## 8. 证据表（执行方填写）

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
