# 交接文档：批次 T（2026-11-20）

> 起点：main 快进到批次 S 复核的最终文档提交（本文随该提交入库）。批次 S 的结论见 [S 复核](REVIEW_2026-11-20-batchS.zh-CN.md)；复核方的命令、运行编号与计数见 [S 交接 §9](HANDOFF-BATCH-S-2026-11-20.zh-CN.md#9-复核方证据2026-11-20)。执行方从本文 §8 证据表开始记录本批的运行编号与计数。
> 每项状态只写在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

## 0. 一句话现状

批次 S 的执行合格。S1 在 Android 模拟器上找出一个 P1 缺陷：Android 不允许普通应用创建硬链接，接收端用硬链接建立最终名的一步因此被拒，自 2026-11-04 起 Android 收不了文件。复核方已改用不依赖硬链接的原子改名（S-N3）。

复核方还做了这几件事：

- 实现了发送端源文件复核期间的活动信号（S-N1）；
- 实现了块阶段磁盘写满的报错（S-N2）；
- 修了接收在绑定之前失败时的报错口径（S-N4）；
- 加了新旧版本互通检查脚本。

批次 T 在本机确认这些改动：

- T0：收尾清理；
- T1：Android 模拟器上重跑 S1；
- T2：Android 应用作为接收端；
- T3：Windows 上的新测试与规模测试；
- T4：新旧版本互通；
- T5：全部门槛。

## 1. 复核方在批次 S 分支上的改动

| 项 | 内容 | 主要位置 |
|---|---|---|
| S-N1 | 新能力位 `CAP_SENDER_KEEPALIVE`（0x0010）：双方都支持时，发送端在阻塞线程池上做 Finalize 前的源文件复核，期间发无操作的 `Control::Resume`；接收端在 Finalize 之前接受并忽略 | `crates/ak-wire`、`crates/ak-engine` |
| S-N2 | 块阶段写入因空间或配额不足失败时，接收端先发 `Error{STORAGE_FULL}`，再等发送端关闭或安静下来 | `crates/ak-engine` |
| S-N3 | Linux 与 Android 用 `renameat2(RENAME_NOREPLACE)` 提交；不支持时退回硬链接；硬链接被拒时用“再查一次再改名”。Windows 不变 | `crates/ak-storage` |
| S-N4 | 接收在绑定之前失败时显示引擎的真实错误（`receiver failed to start`） | `crates/ak-cli` |
| 互通脚本 | `scripts/mixed_version_check.sh`：两个构建在四个方向、两种模式下互传并逐字节比较 | `scripts/` |
| 规模测试 | t13 新增 65,537 个文件、两端空闲时限 4 秒的一项（ignored，release） | `crates/ak-itests` |
| S-N6 | ak-session 配对端到端测试的正向流程窗口放宽（Windows CI 负载高时曾超时） | `crates/ak-session/tests` |
| 文档 | S 复核、S 交接 §9、状态表（S-N1–S-N6 与表尾说明）、STATUS、BACKLOG（P3-02 两项关闭）、PROTOCOL、OPERATIONS 的版本行、支持矩阵 Android 一行 | `docs/` |

## 2. 接手前必读

### 2.1 报完成之前（S-N5）

S 的第一次“完成”回复时，远端还没有本批分支；推送之后，§8 的最终 CI 一行留了占位。本批起：

- 最后一次推送之后，先运行 `git ls-remote --heads origin`，确认远端有 `dev/batch-t-<日期>` 且指向你的最后一个提交；
- 等这次 CI 全部跑完（是跑完，不是开始）再报完成；失败了，先在 §8 记下运行编号、失败的任务与测试、完整报错，再追加一个提交推送，重跑通过也照样记失败那次；
- §8 不留占位；回复里写明最后一次推送的提交号、CI 运行编号与结论。

### 2.2 版本串

本批构建的 `ak-cli --version` 显示 `caps 0x0017 [resume|pause-resume|multistream|sender-keepalive]`，FFI 版本串是 `ak-ffi 1 (wire v1, caps 0x0017)`；`db51363` 及更早的构建显示 0x0007。T1 与 T4 都要贴版本行，用它确认用的是哪个构建。

### 2.3 Android（T1、T2）

- **T1**：沿用 S 交接 §2.2 的做法，把本批的 `ak-cli` 交叉编译到 `x86_64-linux-android`（release），推到 `/data/local/tmp/ak-cli`，再跑与 S1 相同的两次冒烟。
- **T2** 是新增的：让 Android 应用作为接收端。
  - 现有的 `flutter_app/integration_test/j1_android_u1_aux_test.dart` 只覆盖应用发送。照它的做法新增一个接收方向的 runner：宿主上的 `ak-cli` 发送一个合成文件，应用接收；接收完成后，在应用里对收到的文件算 SHA-256，与源文件比对。
  - 只改 `flutter_app/integration_test/`，不改产品代码。需要注册中心时，用本机起的 `ak-cli registry-server`，不连生产服务器。
  - 现有 runner 的配对方式如果不能直接用于接收方向，在 §8 写明卡在哪一步、原因，不要绕过应用的同意流程。
- 两项都是模拟器与宿主同机的测试，§8 里写成“模拟器”，不能写成真机验证。不要把配对码贴进 §8；指纹与 job id 只写前 8 位。
- 失败时记下完整报错，并像 S1 那样在设备上复现、定位，交给复核方，不改产品代码，也不改脚本绕过。

### 2.4 Windows 上的新测试（T3）

- 单元测试，连续跑三次：
  - `cargo test --locked -p ak-engine --lib -- slow_source_check source_check_signals receiver_takes_the_sender_signal disk_full_mid_transfer`（6 项）；
  - `cargo test --locked -p ak-storage -- commit_no_replace hard_link_onto`（Windows 上是这两项；S-N3 新增的两项只在 Unix 上编译，由 CI 的 Linux 任务覆盖）；
  - `cargo test --locked -p ak-cli --bin ak-cli -- a_receive_failing_before_it_binds`。
- 规模测试：`cargo test --locked --release -p ak-itests --test t13_many_files -- --ignored --nocapture --test-threads=1`。
  - 共三项：20,000、65,537，以及 65,537 加 4 秒空闲时限（`T13-65537-idle4s`）。
  - 原样贴上三行 `phases:`。
  - 对 4 秒那一项，算出 `last chunk received` 到 `verify phase started` 的间隔，与 R1 里的约 8 秒对照（R 交接 §8）。

### 2.5 新旧版本互通（T4）

- 旧构建：`git worktree add <临时目录> db51363`，在其中用独立的 target 目录构建，避免与当前树的产物互相覆盖：`CARGO_TARGET_DIR=<临时目录>/target cargo build --locked --release -p ak-cli`。
- 新构建：本批的 release `ak-cli.exe`。
- 运行 `bash scripts/mixed_version_check.sh --old <旧 ak-cli.exe> --new <新 ak-cli.exe>`，原样贴出全部 `[mixed]` 行。
- 用完运行 `git worktree remove <临时目录>`；T0 的分支与 worktree 检查以删除之后的状态为准。

### 2.6 仓库检查

`python scripts/check_repository.py` 的规则同 [S 交接 §2.4](HANDOFF-BATCH-S-2026-11-20.zh-CN.md#24-仓库检查)：STATUS 要链接 docs 下全部交接与复核文档和入口文档；状态表批次行（编号形如 T0、T1、T-N1）的“状态”“证据摘要”两列，以及 STATUS“最新验证”以外的部分，不写运行编号、通过比、测试计数、倍数和带小数或区间的耗时。这些数字只写在本文 §8。

## 3. 全项目未完成事项

### 3.1 所有者事项（agent 不得代办，决定正式完成的时间）

E6 生产部署、C4 物理双机跨 NAT、E5 签名、E7 许可证、CR-P3-03、CR-P3-05，以及 P4-02 的外部安全审查，见 [状态表 §8](REVIEW_2026-11-11.zh-CN.md#8-需所有者确认的事项)。M2 与之后各轮的改动要等 E6 部署后才进入生产服务器；CR-P1-02/05/06 的真机验收、P0-05 的网络样本与 P0-07 的抓包都依赖 C4。C4 里的 Android 手机正是 S-N3 修好的接收方向，真机上要再确认一次。

### 3.2 工程项

- 批次 U：CR-P2-08 生产代码拆分（纯移动，复核方）；Android 后台与生命周期中断在模拟器上的走查；P2-02 的移动数据授权先请所有者决定 v1 是否需要。
- 批次 V：发布前整理。包括安装与卸载、升级兼容（用 `mixed_version_check.sh` 对上一个发布）、ROADMAP、支持矩阵与 BACKLOG 的刷新、发布说明。
- [BACKLOG](BACKLOG.zh-CN.md) P3-02 之下仍开着块阶段的检查点保存一项（默认时限下不构成问题，暂不排）；CR-P3-19 的剩余差距、Linux GUI、Android arm64 真机、BACKLOG 的 P3/P4：同 N 交接 §3.2。

## 4. 批次 T 工作单

| 项 | 步骤 | 验收 |
|---|---|---|
| T0 | 1) `git fetch --prune`；2) 确认 `git merge-base --is-ancestor origin/dev/batch-s-20261120 origin/main` 成立，然后删除这个远端分支；3) 切到 main 并 `git pull --ff-only`，删除本地旧分支，执行 `git worktree prune`；4) 从 main 切出 `dev/batch-t-<日期>`；5) 在 §8 回填 main 上本文所在提交的 CI 运行编号与结论 | `git branch -a` 只剩 main 和本批分支；`git worktree list` 只剩主目录；两条命令的输出原样贴进 §8；§8 有回填行 |
| T1 | 按 §2.3 交叉编译本批 `ak-cli`（release）并推到模拟器，先在设备上跑一次 `ak-cli --version`；再运行 `bash scripts/smoke_android_host.sh --reverse --release`，然后加 `--mb 64` 再跑一次 | 两次都 PASS：Android 接收端报 final ack verified=true，拉回的文件逐字节一致。§8 原样贴出 `--version`、每次的结论行与比较结果 |
| T2 | 按 §2.3 新增应用接收方向的 runner 并在模拟器上运行 | 应用完成接收，SHA-256 与源文件一致；§8 写 runner 的文件名、运行命令、结论行与摘要前 8 位。做不到时写明卡在哪一步与原因 |
| T3 | 按 §2.4 运行单元测试三次与 t13 规模测试一次 | 全部通过；§8 贴三次单元测试的结论行，与规模测试的三行 `phases:` 和间隔对照 |
| T4 | 按 §2.5 运行新旧版本互通 | 八次全部 PASS；§8 原样贴出全部 `[mixed]` 行与两个版本行 |
| T5 | §5 的全部门槛 | 全部通过；失败按 §6 记录 |

并行建议：T3、T4 只用 Windows，T1、T2 要模拟器；同一台机器上先做 T3、T4（规模测试占磁盘和 CPU），再做 T1、T2。

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
- 测试与脚本产生的日志不入库，只在 §8 写结论、耗时和失败时的报错行。
- 缺工具时写“未执行”，不能写成通过。
- 推送前在最终文档上跑一遍 `python scripts/check_repository.py`（§2.6）。

## 6. 文档与分支规则（摘自 AGENTS.md）

- 每项状态只写在状态表里，提交列写真实提交号，不写“本提交”；`check_repository.py` 会检查状态表的每一行，也会拦下被空行隔到表外的行。
- 状态表的“证据摘要”列不写运行编号、计数、倍数和耗时（§2.6 的检查会拦下大部分）。
- 一项被后续批次确认或推翻时，原地改写它的那一行，不要只在表尾追加新行。改写时保留原有的缺陷描述、修复描述和修复提交号，只补充新的结论。S1 行由 T1 的结论原地补充，S-N1 到 S-N4 各行由 T1–T4 的结论原地补充。
- run 号和计数只写在本文 §8；STATUS 只写阶段结论、下一步和链接，并链接到本文。改 STATUS 时整份改写，保留全部入口链接。
- 状态表表尾的说明行由复核方在复核时写，执行方不要加。
- 本文不要同时出现“已核”与“尚待 / 待补 / 仍待 / 待执行”（`check_repository.py` 会拦截）。
- 执行方只推 `dev/batch-t-<日期>`；复核通过后由复核方快进合入 main，再由执行方删除分支。任何人都不强推，包括改写自己刚推上去的提交：发现错误就追加一个新提交。
- 新增的 CI 门槛如果失败，先查清原因；不要为了让它通过而削弱它。
- 失败都要记入 §8，不能只记最后一次：
  - CI 运行失败，记运行编号与失败原因；
  - 测试失败后重跑通过，记失败那次的完整输出与原因分析。
- 报完成之前按 §2.1 确认推送与 CI。
- 验收要求原样贴出的输出，每次都要贴，不能写成“与上一次一致”。
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
