# 交接文档：批次 T（2026-11-20，2026-11-21 重排）

> 起点：main 快进到批次 S 复核的补充文档提交（本文随该提交入库）。批次 S 的结论见 [S 复核](REVIEW_2026-11-20-batchS.zh-CN.md)；复核方的命令、运行编号与计数见 [S 交接 §9](HANDOFF-BATCH-S-2026-11-20.zh-CN.md#9-复核方证据2026-11-20)。执行方从本文 §8 证据表开始记录本批的运行编号与计数。
> 每项状态只写在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。
> 重排原因：执行方在批次 S 合入之后补做了 S1 的复测（模拟器上的 CLI 接收通过），原来的 T1 已经完成，其余各项顺次前移。

## 0. 一句话现状

批次 S 的执行合格。S1 在 Android 模拟器上找出一个 P1 缺陷：Android 不允许普通应用创建硬链接，接收端用硬链接建立最终名的一步因此被拒，自 2026-11-04 起 Android 收不了文件。复核方改用不依赖硬链接的原子改名（S-N3）后，执行方在模拟器上用 CLI 复测通过，`strace` 证实提交走的是 `renameat2(RENAME_NOREPLACE)`。

还没确认的是应用自己作为接收端：CLI 跑在 shell 域，应用跑在普通应用域，SELinux 规则不同。

复核方还做了这几件事：

- 实现了发送端源文件复核期间的活动信号（S-N1）；
- 实现了块阶段磁盘写满的报错（S-N2）；
- 修了接收在绑定之前失败时的报错口径（S-N4）；
- 放宽了配对端到端测试的窗口（S-N6）；
- 加了新旧版本互通检查脚本。

批次 T 在本机确认这些改动：

- T0：收尾清理；
- T1：Android 应用进程内的回环收发（应用作为接收端，模拟器）；
- T2：Windows 上的新测试与规模测试；
- T3：新旧版本互通；
- T4：全部门槛。

## 1. 复核方在批次 S 分支上的改动

| 项 | 内容 | 主要位置 |
|---|---|---|
| S-N1 | 新能力位 `CAP_SENDER_KEEPALIVE`（0x0010）：双方都支持时，发送端在阻塞线程池上做 Finalize 前的源文件复核，期间发无操作的 `Control::Resume`；接收端在 Finalize 之前接受并忽略 | `crates/ak-wire`、`crates/ak-engine` |
| S-N2 | 块阶段写入因空间或配额不足失败时，接收端先发 `Error{STORAGE_FULL}`，再等发送端关闭或安静下来 | `crates/ak-engine` |
| S-N3 | Linux 与 Android 用 `renameat2(RENAME_NOREPLACE)` 提交；不支持时退回硬链接；硬链接被拒时用“再查一次再改名”。Windows 不变 | `crates/ak-storage` |
| S-N4 | 接收在绑定之前失败时显示引擎的真实错误（`receiver failed to start`） | `crates/ak-cli` |
| S-N6 | ak-session 配对端到端测试的正向流程窗口放宽（Windows CI 负载高时曾超时） | `crates/ak-session/tests` |
| 互通脚本 | `scripts/mixed_version_check.sh`：两个构建在四个方向、两种模式下互传并逐字节比较 | `scripts/` |
| 规模测试 | t13 新增 65,537 个文件、两端空闲时限 4 秒的一项（ignored，release） | `crates/ak-itests` |
| 文档 | S 复核、S 交接 §9、状态表（S1、S-N1–S-N6、main 合并与表尾说明）、STATUS、BACKLOG（P3-02 两项关闭）、PROTOCOL、OPERATIONS 的版本行、支持矩阵 Android 一行 | `docs/` |

## 2. 接手前必读

### 2.1 报完成之前（S-N5）与分支

批次 S 的第一次“完成”回复时，远端还没有本批分支；推送之后，§8 的最终 CI 一行留了占位。本批起：

- 最后一次推送之后，先运行 `git ls-remote --heads origin`，确认远端有 `dev/batch-t-<日期>` 且指向你的最后一个提交；
- 等这次 CI 全部跑完（是跑完，不是开始）再报完成；失败了，先在 §8 记下运行编号、失败的任务与测试、完整报错，再追加一个提交推送，重跑通过也照样记失败那次；
- §8 不留占位；回复里写明最后一次推送的提交号、CI 运行编号与结论。

批次 S 合入 main 之后，执行方又向 `dev/batch-s-20261120` 推了一个记录提交（S1 的复测），复核方只好再合一次。本批起，一批合入 main 之后，与它有关的新记录（包括所有者临时要求的复测）都写进下一批的分支与交接，不再推到已合入的分支。

### 2.2 版本串

本批构建的 `ak-cli --version` 显示 `caps 0x0017 [resume|pause-resume|multistream|sender-keepalive]`，FFI 版本串是 `ak-ffi 1 (wire v1, caps 0x0017)`；`db51363` 及更早的构建显示 0x0007。T1 与 T3 都要贴版本串，用它确认用的是哪个构建。

### 2.3 Android 应用进程内的回环收发（T1）

为什么这样测：

- S1 的复测用的是 adb 推上去的 CLI，跑在 shell 域；应用跑在普通应用域（untrusted_app），硬链接正是对这类域一律禁止的，所以应用自己的接收要单独确认。
- 用宿主 CLI 当发送端、应用当接收端走不通：模拟器的 NAT 不放行宿主主动连入。应用通告的是模拟器内部地址，宿主连不上；`smoke_android_host.sh` 为 CLI 搭的 UDP 转发对任务码流程不适用（原因见脚本开头的 TRANSPORT NOTE）。
- 所以在应用进程里同时起接收端与发送端，经 FFI 走 127.0.0.1 的真实 QUIC 回环。接收目录放在应用自己的数据目录里，提交步骤就在应用的域里执行。这是真实收发路径，不是测试接缝。

怎么做：

- 先在本批分支上运行 `bash scripts/build_android_ffi.sh`，把含 S-N3 的 `libak_ffi.so` 重新构建并放进 `jniLibs/`；§8 记下脚本最后打印的 x86_64 一行（大小与 SHA-256 前 8 位）。jniLibs 里的 .so 不入库（`flutter_app/.gitignore`），旧的 .so 没有 S-N3。
- 新增 `flutter_app/integration_test/t1_android_app_receive_test.dart`，只改 `flutter_app/integration_test/`，不改产品代码。
- 打开动态库照 `integration_test/android_real_test.dart` 的做法：`AkEngineDartFacade()` 的 `borrowEngine()` 拿到引擎（Android 上按 `lib/services/dll_path.dart` 打开 apk 里的 `libak_ffi.so`），结束时 `facade.dispose()`。不要用 `AkEngineDart()` 的默认路径，那是桌面开发用的相对路径。测试开头打印 `engine.version()`，确认是 0x0017。
- 接收端身份：在本轮的临时目录里用 `engine.identityInfo(<身份目录>)` 新建一个身份，它的 `fingerprint` 就是发送端的 `fpHex`。身份目录只留在设备上，每轮结束删除；日志里的指纹只打印前 8 位。
- 回环收发照 `flutter_app/packages/ak_ffi_dart/test/ffi_bindings_test.dart` 里“loopback transfer”一项的做法：`receiveStart`（`port: 0`，`expect: 'any'`，`idDir` 用上面的身份目录）→ 等 listening 事件拿到端口 → `sendStart` 到 `127.0.0.1:<端口>` → 轮询两边的 `jobResult` 直到都结束。
- 源目录与接收目录都放在 `getApplicationSupportDirectory()` 之下。源数据在测试里按固定规律生成：
  - 一个 8 MiB 文件；
  - 一个中文文件名的小文件；
  - 一个空目录；
  - 一个两层的子目录。
  不用任何用户文件。
- 判定：两边都报完成；对源与接收目录里的每个文件算 SHA-256，逐个相等；空目录与子目录结构一致。
- 运行：`flutter test integration_test/t1_android_app_receive_test.dart -d <模拟器>`。
  - 每轮都在新的临时目录里重建源与接收目录，不复用检查点；
  - 连续跑两轮；
  - 再把 8 MiB 换成 64 MiB 跑一轮。
- §8 记录：
  - 版本串；
  - 每轮的结论行；
  - 每个文件的大小与 SHA-256 前 8 位。
  - 可选：在设备上对应用进程做 `strace` 看提交调用。只有能做到时才做，不 root、不改 SELinux 模式；做不到就写“未做”和原因。
- 这是模拟器上同一进程内的回环，§8 里写成“模拟器”，不能写成真机验证。失败时记下完整报错，在设备上复现、定位，交给复核方，不改产品代码。

### 2.4 Windows 上的新测试（T2）

- 单元测试，连续跑三次：
  - `cargo test --locked -p ak-engine --lib -- slow_source_check source_check_signals receiver_takes_the_sender_signal disk_full_mid_transfer`（6 项）；
  - `cargo test --locked -p ak-storage -- commit_no_replace hard_link_onto`（Windows 上是这两项；S-N3 新增的两项只在 Unix 上编译，由 CI 的 Linux 任务覆盖）；
  - `cargo test --locked -p ak-cli --bin ak-cli -- a_receive_failing_before_it_binds`。
- 规模测试：`cargo test --locked --release -p ak-itests --test t13_many_files -- --ignored --nocapture --test-threads=1`。
  - 共三项：20,000、65,537，以及 65,537 加 4 秒空闲时限（`T13-65537-idle4s`）。
  - 原样贴上三行 `phases:`。
  - 对 4 秒那一项，算出 `last chunk received` 到 `verify phase started` 的间隔，与 R1 里的约 8 秒对照（R 交接 §8）。

### 2.5 新旧版本互通（T3）

- 旧构建：`git worktree add <临时目录> db51363`，在其中用独立的 target 目录构建，避免与当前树的产物互相覆盖：`CARGO_TARGET_DIR=<临时目录>/target cargo build --locked --release -p ak-cli`。
- 新构建：本批的 release `ak-cli.exe`。
- 运行 `bash scripts/mixed_version_check.sh --old <旧 ak-cli.exe> --new <新 ak-cli.exe>`，原样贴出全部 `[mixed]` 行。
- 用完运行 `git worktree remove <临时目录>`；T0 的分支与 worktree 检查以删除之后的状态为准。

### 2.6 仓库检查

`python scripts/check_repository.py` 的规则同 [S 交接 §2.4](HANDOFF-BATCH-S-2026-11-20.zh-CN.md#24-仓库检查)：STATUS 要链接 docs 下全部交接与复核文档和入口文档；状态表批次行（编号形如 T0、T1、T-N1）的“状态”“证据摘要”两列，以及 STATUS“最新验证”以外的部分，不写运行编号、通过比、测试计数、倍数和带小数或区间的耗时。这些数字只写在本文 §8。

## 3. 全项目未完成事项

### 3.1 所有者事项（agent 不得代办，决定正式完成的时间）

E6 生产部署、C4 物理双机跨 NAT、E5 签名、E7 许可证、CR-P3-03、CR-P3-05，以及 P4-02 的外部安全审查，见 [状态表 §8](REVIEW_2026-11-11.zh-CN.md#8-需所有者确认的事项)。M2 与之后各轮的改动要等 E6 部署后才进入生产服务器；CR-P1-02/05/06 的真机验收、P0-05 的网络样本与 P0-07 的抓包都依赖 C4。C4 里的 Android 手机走的正是 S-N3 修好的接收方向，真机上要再确认一次。

### 3.2 工程项

- 批次 U：CR-P2-08 生产代码拆分（纯移动，复核方）；Android 后台与生命周期中断在模拟器上的走查；P2-02 的移动数据授权先请所有者决定 v1 是否需要。
- 批次 V：发布前整理。包括安装与卸载、升级兼容（用 `mixed_version_check.sh` 对上一个发布）、ROADMAP、支持矩阵与 BACKLOG 的刷新、发布说明。
- [BACKLOG](BACKLOG.zh-CN.md) P3-02 之下仍开着块阶段的检查点保存一项（默认时限下不构成问题，暂不排）；CR-P3-19 的剩余差距、Linux GUI、Android arm64 真机、BACKLOG 的 P3/P4：同 N 交接 §3.2。

## 4. 批次 T 工作单

| 项 | 步骤 | 验收 |
|---|---|---|
| T0 | 1) `git fetch --prune`；2) 确认 `git merge-base --is-ancestor origin/dev/batch-s-20261120 origin/main` 成立，然后删除这个远端分支；3) 切到 main 并 `git pull --ff-only`，删除本地旧分支，执行 `git worktree prune`；4) 从 main 切出 `dev/batch-t-<日期>`；5) 在 §8 回填 main 上本文所在提交的 CI 运行编号与结论 | `git branch -a` 只剩 main 和本批分支；`git worktree list` 只剩主目录；两条命令的输出原样贴进 §8；§8 有回填行 |
| T1 | 按 §2.3 重建 Android 的 `libak_ffi.so`，新增应用进程内回环收发的 integration test，在模拟器上跑三轮（两轮 8 MiB、一轮 64 MiB） | 三轮都完成，每个文件的 SHA-256 逐个相等，目录结构一致；§8 贴 .so 的构建行、版本串、每轮结论行、每个文件的大小与摘要前 8 位。失败时贴完整报错与定位，不改产品代码 |
| T2 | 按 §2.4 运行单元测试三次与 t13 规模测试一次 | 全部通过；§8 贴三次单元测试的结论行，与规模测试的三行 `phases:` 和间隔对照 |
| T3 | 按 §2.5 运行新旧版本互通 | 八次全部 PASS；§8 原样贴出全部 `[mixed]` 行与两个版本行 |
| T4 | §5 的全部门槛 | 全部通过；失败按 §6 记录 |

并行建议：T2、T3 只用 Windows，T1 要模拟器；同一台机器上先做 T2、T3（规模测试占磁盘和 CPU），再做 T1。

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
- T1 新增了 Dart 文件，提交前在 `flutter_app` 下跑一次 `flutter analyze`。
- 测试与脚本产生的日志不入库，只在 §8 写结论、耗时和失败时的报错行。
- 缺工具时写“未执行”，不能写成通过。
- 推送前在最终文档上跑一遍 `python scripts/check_repository.py`（§2.6）。

## 6. 文档与分支规则（摘自 AGENTS.md）

- 每项状态只写在状态表里，提交列写真实提交号，不写“本提交”；`check_repository.py` 会检查状态表的每一行，也会拦下被空行隔到表外的行。
- 状态表的“证据摘要”列不写运行编号、计数、倍数和耗时（§2.6 的检查会拦下大部分）。
- 一项被后续批次确认或推翻时，原地改写它的那一行，不要只在表尾追加新行。改写时保留原有的缺陷描述、修复描述和修复提交号，只补充新的结论。S-N3 一行由 T1 的结论原地补充，S-N1、S-N2、S-N4 各行由 T2、T3 的结论原地补充。
- run 号和计数只写在本文 §8；STATUS 只写阶段结论、下一步和链接，并链接到本文。改 STATUS 时整份改写，保留全部入口链接。
- 状态表表尾的说明行由复核方在复核时写，执行方不要加。
- 本文不要同时出现“已核”与“尚待 / 待补 / 仍待 / 待执行”（`check_repository.py` 会拦截）。
- 执行方只推 `dev/batch-t-<日期>`；复核通过后由复核方快进合入 main，再由执行方删除分支。任何人都不强推，包括改写自己刚推上去的提交：发现错误就追加一个新提交。合入之后的新记录写进下一批（§2.1）。
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

执行方在本机 Windows 上工作（rustc 1.99.1，与 `rust-toolchain.toml` 一致）；T2、T3 先做，T1 最后（模拟器）。T1 的模拟器是本机 AVD `ak_test`（Android 15，API 35，userdebug，SELinux Enforcing，x86_64）——以下 T1 各条都是模拟器上同一进程内的回环记录，不是真机验证；配对码不入库，指纹与 job id 只写前 8 位。

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
| main 上批次 S 最终提交的 CI | `095f7d8` run [44776125791](https://github.com/wmqfl861/AK-sending/actions/runs/44776125791) | 第 1 次尝试 completed **failure**：唯一失败的任务是 Rust 1.99.1 (windows-latest)，ak-itests `t06_identity_swap` 的 `mid_transfer_port_takeover_by_another_key_is_refused_by_pinning`（`impostor must take over the freed port`，T 交接预告过的已知测试余量问题）。第 2 次尝试只重跑该任务，completed success（12 项常规任务 success，2 项手动深测 skipped）。结论以 API 核实为准，两次都记 |
| T0 分支与 worktree | 本地 Windows | `git merge-base --is-ancestor origin/dev/batch-s-20261120 origin/main` 成立后删除了该远端分支；本地 main 快进到 `095f7d8`，删除本地 `dev/batch-s-20261120`，`git worktree prune` 后从 main 切出 `dev/batch-t-20261121`。`git branch -a` 原样输出：`* dev/batch-t-20261121`、`  main`、`  remotes/origin/HEAD -> origin/main`、`  remotes/origin/main`。`git worktree list` 原样输出（本机仓库根路径按红线略写为 `<repo>`）：`<repo> 095f7d8 [dev/batch-t-20261121]`，只有主目录一行（T3 的临时 worktree 已按 §2.5 删除后再查） |
| T2 单元测试第 1 轮 | 本地 Windows | 三组全过：`test result: ok. 6 passed; 0 failed`（ak-engine）、`test result: ok. 2 passed; 0 failed`（ak-storage，Windows 上只编这两项）、`test result: ok. 1 passed; 0 failed`（ak-cli） |
| T2 单元测试第 2 轮 | 本地 Windows | 三组全过：`ok. 6 passed; 0 failed`、`ok. 2 passed; 0 failed`、`ok. 1 passed; 0 failed` |
| T2 单元测试第 3 轮 | 本地 Windows | 三组全过：`ok. 6 passed; 0 failed`、`ok. 2 passed; 0 failed`、`ok. 1 passed; 0 failed` |
| T2 规模测试 | 本地 Windows（818.61s，三项全过） | `T13-20000: phases: manifest received @ 8.1s, first chunk sent @ 24.1s, last chunk received @ 35.6s, verify phase started @ 37.9s, final ack @ 65.0s, final ack @ 65.0s, done @ 65.1s, done @ 65.1s`，校验 37.9→65.0 = 27.1s（R1 同机 23.9s/26.0s，一致）；`T13-65537: phases: manifest received @ 25.6s, first chunk sent @ 65.9s, last chunk received @ 105.4s, verify phase started @ 113.2s, final ack @ 206.8s, final ack @ 206.8s, done @ 206.9s, done @ 206.9s`，校验 113.2→206.8 = 93.6s（R1 同机 88.9s/92.4s，一致）；`T13-65537-idle4s: phases: manifest received @ 26.9s, first chunk sent @ 74.3s, last chunk received @ 115.7s, verify phase started @ 124.0s, final ack @ 219.6s, final ack @ 219.6s, done @ 219.6s, done @ 219.7s`，last chunk received 到 verify phase started 的间隔 115.7→124.0 = **8.3s**，与 R1 的约 8.0s 同量级——源文件复核这一段超过了 4 秒的空闲时限而发送端没有掉线，S-N1 的活动信号在 65,537 文件规模下起效 |
| T3 新旧版本互通 | 本地 Windows | 旧构建：`git worktree add <临时目录> db51363`，独立 `CARGO_TARGET_DIR` 构建（2m34s）；新构建：本批 release。`[mixed] old: ak-cli 0.1.0 (wire v1, caps 0x0007 [resume|pause-resume|multistream], release build, git db51363)`；`[mixed] new: ak-cli 0.1.0 (wire v1, caps 0x0017 [resume|pause-resume|multistream|sender-keepalive], release build, git 095f7d8)`；`[mixed] new->new single-stream: PASS (job f181a377, tree identical)`；`[mixed] new->new --multistream: PASS (job f181a377, tree identical)`；`[mixed] new->old single-stream: PASS (job f181a377, tree identical)`；`[mixed] new->old --multistream: PASS (job f181a377, tree identical)`；`[mixed] old->new single-stream: PASS (job f181a377, tree identical)`；`[mixed] old->new --multistream: PASS (job f181a377, tree identical)`；`[mixed] old->old single-stream: PASS (job f181a377, tree identical)`；`[mixed] old->old --multistream: PASS (job f181a377, tree identical)`；`[mixed] RESULT: PASS (8/8 runs)`。用完已 `git worktree remove`（T0 行的 worktree 检查以删除后为准） |
| T1 FFI 构建 | 本地 Windows，`bash scripts/build_android_ffi.sh` | 成功。脚本输出原样：`==> x86_64    libak_ffi.so     9698120 bytes  sha256 d5b06e07a7836ba455f13c5bb2220fcd6add11f9e3e142362c949b765fc2cbca`（arm64-v8a 一并构建；ABI 导出符号校验通过；已放入 jniLibs，不入库） |
| T1 第 1 轮（8 MiB） | 模拟器（应用进程内回环） | **PASS**（`All tests passed!`，退出码 0）。`engine.version(): ak-ffi 1 (wire v1, caps 0x0017)`；接收端指纹 14a2d292；`both jobs done: files=6 bytes=8388663`；`payload.bin: 8388608 bytes, sha256(8) a37b426c`；`说明-中文文件名.txt: 40 bytes, sha256(8) 5abef6f0`；`nested/level2/inner.txt: 15 bytes, sha256(8) 4e7267fd`；空目录与两层子目录结构一致；`PASS: 8 MiB round — SHA-256 equal per file, layout equal`。源与接收目录都在 `getApplicationSupportDirectory()` 之下，提交步骤在应用的 untrusted_app 域里执行 |
| T1 第 2 轮（8 MiB） | 模拟器（应用进程内回环） | **PASS**（退出码 0）。指纹 01908fec；`files=6 bytes=8388663`；三个文件的 SHA-256 与第 1 轮逐字相同（a37b426c / 5abef6f0 / 4e7267fd，源数据按固定种子生成）；`PASS: 8 MiB round — SHA-256 equal per file, layout equal` |
| T1 第 3 轮（64 MiB） | 模拟器（应用进程内回环） | **PASS**（退出码 0）。指纹 c2fc96f7；`both jobs done: files=6 bytes=67108920`；`payload.bin: 67108864 bytes, sha256(8) cfdb793c`；`说明-中文文件名.txt: 41 bytes, sha256(8) ba3040f9`；`nested/level2/inner.txt: 15 bytes, sha256(8) 4e7267fd`；`PASS: 64 MiB round — SHA-256 equal per file, layout equal` |
| T1 过程记录 | 本地 Windows + 模拟器 | （1）`flutter analyze` 首次报 4 处（1 error：`FileMode` 未导入；1 error：`result.map` 可空返回；1 warning：多余判空；1 info：文档注释尖括号）与 1 处漏改，全部修掉后 `No issues found!`；（2）第 1 轮首次运行因本机 `JAVA_HOME` 未设置失败（`Gradle task assembleDebug failed`，已指向本机 jdk 重试）；（3）Gradle 9.3.1 发行版下载两次停滞（40MB、95MB 处），终止后用 curl 断点续传补齐并手工就位，属本机环境问题，不是仓库问题；（4）测试自身两处笔误（子目录未递归创建、`_awaitListening` 漏 `await`）各失败一次后修复，失败输出都已核对，不改产品代码；（5）可选的 strace 取证**未做**：应用进程属 untrusted_app 域，adb shell 的 strace（以 shell 身份）无法附着，不 root、不改 SELinux 模式就做不到，按 T 交接 §2.3 的口径写明原因 |
| T4 全量门槛 | 本地 Windows（rustc 1.99.1） | fmt/clippy/rustdoc/workspace（69 组，1231/0/11，较批次 S 多出的项来自复核方的新测试）/sqlite（54/0）/server-persistence（221/0）/Python（372 OK）/repo/contracts（5/5 PASS）/部署自检（PASS）/跨NAT自检（PASS）全部通过；`flutter analyze`（flutter_app 下，含新增的 T1 测试文件）`No issues found!`；`check_repository.py` 在回填本表提交号后通过（回填前只报状态表提交列占位） |
| T 批次最终 CI | `5c7c742` run [44870317557](https://github.com/wmqfl861/AK-sending/actions/runs/44870317557) | **completed success**（12 项常规任务 success，2 项手动深测 skipped） |

## 9. 复核方证据（2026-11-21）

复核方在云端 Linux 工作区（2 vCPU，rustc 1.97.0，钉的 1.99.1 下载不到）执行；Windows 上的结果只来自 CI，Android 上的结果来自 §8。

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
| 分支活动 | GitHub activity API | 2026-11-21T01:26:20Z 删除 `dev/batch-s-20261120`（`095f7d8`，main 此时也是 `095f7d8`）；05:19:05Z 一次推送创建 `dev/batch-t-20261121` @ `5c7c742`（含 `7d010d4`、`5c7c742`）；05:33:21Z 推送 `61b3143`；之后复核方推送 `0b8c288`（T-N1、T-N2）与复核文档提交。全部是快进的普通推送，没有强推。执行方报完成是本地 14:12（06:12Z），此时最后一次推送的 CI 已于 05:45:21Z 跑完 |
| 执行方推送的 CI | `5c7c742` run [44870317557](https://github.com/wmqfl861/AK-sending/actions/runs/44870317557) | success：12 项常规任务 success，2 项手动深测 skipped（§8 最后一行与之一致） |
| 执行方最后一次推送的 CI | `61b3143` run [44871426085](https://github.com/wmqfl861/AK-sending/actions/runs/44871426085) | success：12 项常规任务 success，2 项手动深测 skipped |
| main 上 `095f7d8` 的 CI | run [44776125791](https://github.com/wmqfl861/AK-sending/actions/runs/44776125791) | 第 1 次尝试 failure：只有 Rust 1.99.1 (windows-latest)，t06 的 `mid_transfer_port_takeover_by_another_key_is_refused_by_pinning`（`impostor must take over the freed port`）；第 2 次尝试只重跑该任务，success。§8 第一行与 API 一致 |
| T-N1 端口释放时间 | Linux 云端工作区，debug 构建，t06 场景 (a) 加临时计时（未提交），每组 10 次 | 当前树（`095f7d8`）：空闲时每次都在第 4 次尝试绑上，305–308 ms；3 个占满 CPU 的进程下 5–8 次尝试，412–716 ms。旧树 `db51363`（独立 worktree 与独立 target）：空闲 305–308 ms；同样负载下 4–7 次尝试，305–611 ms。两棵树分布相同。原测试的上限是 20 次尝试，约 2 s 加绑定开销 |
| T-N1 修改后 | `0b8c288` | 整个 t06 测试文件连续 20 次通过：10 次空闲，10 次在 4 个占满 CPU 的进程下。截止时间临时改为 50 ms 时测试失败，报错原文 `impostor must take over the freed port: still taken after 100.784107ms (1 attempts)`；改动已还原、未提交 |
| T-N2 | `0b8c288` | 本工作区没有 Flutter SDK，Dart 改动未在本地分析或运行；由 CI 的 Flutter gate（`flutter analyze`）检查，U1 在模拟器上重跑 |
| S-N3 的 seccomp 核对 | AOSP bionic | android-8.0.0_r1 的 [`libc/SECCOMP_WHITELIST.TXT`](https://android.googlesource.com/platform/bionic/+/refs/tags/android-8.0.0_r1/libc/SECCOMP_WHITELIST.TXT) 在 `# Useful new syscalls which we don't yet use in bionic.` 之下列有 `renameat2`；按 [blame](https://gerrit.omnirom.org/plugins/gitiles/android_bionic/+blame/a34817457feee026e8702a1d2dffe9e92b51d7d1/libc/SECCOMP_WHITELIST.TXT)，这一行由 `de9262e`（2017-04-18）加入，文件头由 `3dd3d55`（2017-04-12）建立。当前 bionic 的 [`libc/SYSCALLS.TXT`](https://android.googlesource.com/platform/bionic/+/HEAD/libc/SYSCALLS.TXT) 有 `renameat2(int, const char*, int, const char*, unsigned) all`。应用的 seccomp 过滤自 Android 8.0 起才有 |
| 本地全部门槛 | `0b8c288` 加本次文档 | 全部通过（789s）：fmt、clippy、workspace 69 组 1231 passed / 0 failed / 11 ignored、rustdoc、ak-store sqlite 52 passed、ak-signal server-persistence 221 passed、Python 372 OK、check_contracts、部署自检、跨 NAT 自检。check_repository 在这次运行里报了三处，都是新文档还没写进 STATUS，写完 STATUS 后通过 |
| 复核方推送的 CI | — | 复核文档提交随本文入库，它的运行编号由执行方在 U0 回填 |
