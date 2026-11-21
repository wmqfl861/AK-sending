# 交接文档：批次 U（2026-11-21）

> 起点：main 快进到批次 T 复核的最终文档提交，本文随该提交入库。
>
> - 批次 T 的结论见 [T 复核](REVIEW_2026-11-21-batchT.zh-CN.md)；
> - 复核方的命令、运行编号与计数见 [T 交接 §9](HANDOFF-BATCH-T-2026-11-20.zh-CN.md#9-复核方证据2026-11-21)；
> - 执行方从本文 §8 证据表开始记录本批的运行编号与计数；
> - 每项状态只写在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

## 0. 一句话现状

批次 T 通过：

- 应用自己作为接收端在模拟器上可用，S-N3 在应用一侧得到确认；
- S-N1、S-N2、S-N4 在 Windows 上确认，包括 65,537 个文件、4 秒空闲时限的规模测试；
- 新旧版本互通八项全部通过。

复核方改了两处测试，并登记了一项构建环境问题：

- T-N1：t06 等端口释放改为截止时间；
- T-N2：T1 测试的版本检查只查能力位；
- T-N3：Gradle wrapper 没有固定发行包的校验值。

批次 U：

- U0：收尾清理，回填 main 的 CI，核对本机的 Gradle 发行包；
- U1：Android 接收方向的每批回归；
- U2：Android 后台与生命周期走查；
- U3：Windows 上连续运行 t06，确认 T-N1；
- U4：全部门槛。

复核方在 U 复核时做 CR-P2-08 的生产代码拆分（纯移动），由批次 V 在 Windows 上确认。本批进行期间，复核方不向本批分支推送。

## 1. 复核方在批次 T 分支上的改动

| 项 | 内容 | 主要位置 |
|---|---|---|
| T-N1 | t06 场景 (a) 绑定被中止接收端的端口，原来只重试约 2 秒，改为重试到 30 秒的截止时间；端口一释放就绑上，始终不释放时报错里带等待时长与尝试次数 | `crates/ak-itests/tests/t06_identity_swap.rs` |
| T-N2 | T1 测试原来要求版本串含 `0x0017`，改为只要求含 `CAP_SENDER_KEEPALIVE`（0x0010）这一位，以后加能力位不会误报 | `flutter_app/integration_test/t1_android_app_receive_test.dart` |
| 文档 | T 复核；T 交接 §9；状态表（T0、S-N3、T-N1 至 T-N3、main 合并与表尾说明）；STATUS；支持矩阵的 Android 一行 | `docs/` |

## 2. 接手前必读

### 2.1 报完成之前与分支

沿用 T 交接 §2.1，批次 T 都做到了：

- 最后一次推送之后，先运行 `git ls-remote --heads origin`，确认远端有 `dev/batch-u-<日期>` 且指向你的最后一个提交；
- 等这次 CI 全部跑完再报完成；
- 失败了，先在 §8 记下运行编号、失败的任务与测试、完整报错，再追加一个提交推送，重跑通过也照样记失败那次；
- §8 不留占位；回复里写明最后一次推送的提交号、CI 运行编号与结论；
- 一批合入 main 之后，与它有关的新记录写进下一批的分支与交接，不再推到已合入的分支。

### 2.2 Gradle 发行包（U0，T-N3）

批次 T 里，Gradle 9.3.1 的发行包下载停滞，执行方用 curl 断点续传补齐后手工放进 wrapper 的目录。仓库的 `gradle-wrapper.properties` 没有 `distributionSha256Sum`，所以这个包没有人核对过。

做法：

- 找到本机 wrapper 目录里的 `gradle-9.3.1-all.zip`，在 `GRADLE_USER_HOME` 的 `wrapper/dists` 之下，§8 里路径按红线略写；
- 用 `sha256sum`（Git Bash）或 `certutil -hashfile <zip> SHA256` 算出它的 SHA-256；
- 从 `https://services.gradle.org/distributions/gradle-9.3.1-all.zip.sha256` 取官方值；
- 两个值原样贴进 §8。

两个值不一致时，删除这个版本的整个解包目录与 zip，让 wrapper 重新下载，再比一次，§8 照实记录。不要改 `gradle-wrapper.properties`，校验值由复核方写入。

### 2.3 Android 接收方向的每批回归（U1）

- 先运行 `bash scripts/build_android_ffi.sh`，§8 记下脚本最后打印的 x86_64 一行。
- 再运行 `flutter test integration_test/t1_android_app_receive_test.dart -d <模拟器>`，默认 8 MiB，跑一轮。
- §8 贴出：`engine.version()` 一行、结论行、每个文件的大小与 SHA-256 前 8 位。
- T-N2 改过这个测试的版本检查，这一轮同时确认它。

### 2.4 Android 后台与生命周期走查（U2）

为什么做：

- [存储与平台](STORAGE_AND_PLATFORMS.zh-CN.md) 要求“手机锁屏/后台/被系统终止/用户强制退出分开验证；不承诺全情形常驻”；
- [PRD](PRD.zh-CN.md) 的 FR-009 要求后台中断测试；
- [BACKLOG](BACKLOG.zh-CN.md) 的 P0-06 一直没有走查。

应用的数据面跑在引擎的原生线程里，不在 Dart 里。所以关键是两件事：应用进程在后台还能不能继续跑；被杀之后，再发同一个文件时是续传还是从头开始。

#### 方向与环境

- **方向**：应用发送，宿主的 CLI 接收。反方向，即宿主连入模拟器，被模拟器的 NAT 挡住（见 T 交接 §2.3），本批不测。
- **runner**：以 `flutter_app/integration_test/j1_android_u1_aux_test.dart` 为底本（真实的生产启动流程与任务码流程），新增 `flutter_app/integration_test/u2_android_lifecycle_test.dart`。只改 `flutter_app/integration_test/`，不改产品代码。
- **本地服务**：照 [U1 交接 §5](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md#5-本地-agent-走查清单j1) 的“准备”一节，在宿主起 `ak-cli registry-server` 与 `ak-cli stun-server`。应用经 `--dart-define` 取得 `AK_REGISTRY_ADDR=10.0.2.2:17200`、`AK_STUN_ADDR=10.0.2.2:17101` 与 `AK_REGISTRY_ID`。
- **宿主接收端**：`ak-cli --id-dir <宿主身份目录> task join-recv <任务码> --out <接收目录> --server 127.0.0.1:17200 --registry-fp <注册中心指纹> --stun 127.0.0.1:17101`，然后同意接收。`--stun` 必须显式给出：默认值是生产服务器。
- **载荷**：在应用的数据目录里按固定规律生成一个约 200 MiB 的合成文件，大小以传输能持续一两分钟为准，太快就加大。

#### runner 的三个要求

1. 应用在后台时不要调用 `tester.pump`：后台没有帧，pump 会卡住。等待用 `Future.delayed`。传输状态以服务层的进度流和宿主接收端的输出为准，每隔几秒打印一行带时间戳的进度（阶段、已传字节）。
2. 用一个 `--dart-define` 开关决定是否保留发送端的检查点。底本在每次发送前删除检查点；续传的第二段要保留。
3. 源文件已存在且大小正确时不要重写。job id 包含文件的修改时间，重写之后就是另一个任务，没法续传。

#### 开始前记录

- `adb shell getprop ro.build.version.sdk`；
- `adb shell settings get global cached_apps_freezer`。

#### 四种情形

每种情形都从头开始：新的任务码、新的宿主接收目录、清掉发送端检查点。只有 L3、L4 的第二段例外。

| 情形 | 操作 | 记录 |
|---|---|---|
| L1 切到后台 | 宿主接收端进度过了约 20% 时，执行 `adb shell input keyevent KEYCODE_HOME`。60 秒后执行 `adb shell am start -n dev.ak.ak_sending_app/.MainActivity` 回到前台 | 切出时、30 秒、60 秒三个时刻宿主接收端的进度（是否还在增长）；回到前台之后的最终结论 |
| L2 锁屏 | 同样在约 20% 时执行 `adb shell input keyevent KEYCODE_POWER` 灭屏。60 秒后执行 `adb shell input keyevent KEYCODE_WAKEUP` 与 `adb shell wm dismiss-keyguard` | 同 L1 |
| L3 系统回收 | 先按 L1 切到后台，10 秒后执行 `adb shell am kill dev.ak.ak_sending_app`，它只杀后台、可回收的进程，相当于内存回收 | 宿主接收端最后的报错原文与出现的时间；接收目录里留下了什么（只写文件名后缀与大小，例如 `.akpart`、staging 目录）；`flutter test` 的输出（预期与应用失去连接）。然后做第二段，见表下说明 |
| L4 强制停止 | 传输进行中（前台）执行 `adb shell am force-stop dev.ak.ak_sending_app` | 同 L3，也做第二段 |

L3、L4 的第二段：

- 用保留检查点的开关重新运行 runner，发送同一个文件；
- 任务码是新的；宿主用同一个 `--id-dir` 和同一个 `--out` 加入；
- 记录这一次是续传还是从头开始（两端打印的 resuming 一类的行原样贴），以及最终的 SHA-256。

#### 判定与边界

- 每种情形都要有：执行的命令原文与时间、进度采样、宿主接收端最后几行原文；完成时，接收文件的 SHA-256 与源文件比对。
- 这是走查，记录实际行为。后台期间传输停住、失败，或第二段从头开始，都不是执行方的失败，照实记录，交复核方决定，例如是否需要前台服务。不要为了让传输继续而改产品代码或系统设置。
- 不 root，不改 SELinux 模式，不用 `dumpsys deviceidle force-idle` 一类强制进入省电状态的命令。
- `flutter test` 跑的是 debug 构建。§8 的结论里写明这一点：release 构建的行为可能不同。
- 这是模拟器与宿主同机的测试，§8 里写成“模拟器”，不能写成真机验证。

### 2.5 Windows 上的 t06（U3）

在 Git Bash 里运行：

```sh
for i in $(seq 1 20); do
  cargo test --locked -p ak-itests --test t06_identity_swap 2>&1 | grep "test result"
done
```

§8 贴出 20 行结论与通过次数。有失败时，贴完整输出，不要改测试。

### 2.6 仓库检查

`python scripts/check_repository.py` 的规则同 [S 交接 §2.4](HANDOFF-BATCH-S-2026-11-20.zh-CN.md#24-仓库检查)：

- STATUS 要链接 docs 下全部交接与复核文档，以及入口文档；
- 状态表批次行（编号形如 U0、U1、U-N1）的“状态”“证据摘要”两列，以及 STATUS“最新验证”以外的部分，不写运行编号、通过比、测试计数、倍数和带小数或区间的耗时。这些数字只写在本文 §8。

## 3. 全项目未完成事项

### 3.1 所有者事项（agent 不得代办，决定正式完成的时间）

- E6 生产部署、C4 物理双机跨 NAT、E5 签名、E7 许可证、CR-P3-03、CR-P3-05，以及 P4-02 的外部安全审查，见 [状态表 §8](REVIEW_2026-11-11.zh-CN.md#8-需所有者确认的事项)。
- M2 与之后各轮的改动，要等 E6 部署后才进入生产服务器。
- CR-P1-02/05/06 的真机验收、P0-05 的网络样本与 P0-07 的抓包都依赖 C4；C4 里的 Android 手机要再确认一次接收方向（S-N3）。
- P2-02 的移动数据授权：请所有者决定 v1 是否需要。

### 3.2 工程项

- 复核方，U 复核时：
  - CR-P2-08 的生产代码拆分；
  - 把 Gradle 发行包的官方校验值写进 wrapper（T-N3）；
  - 按 U2 的结果决定 Android 的后台策略。
- 批次 V：发布前整理，内容如下：
  - 确认 CR-P2-08 的拆分；
  - 安装与卸载；
  - 升级兼容：用 `mixed_version_check.sh` 对上一个发布；
  - 刷新 ROADMAP、支持矩阵与 BACKLOG；
  - 发布说明。
- [BACKLOG](BACKLOG.zh-CN.md) P3-02 之下仍开着块阶段的检查点保存一项，默认时限下不构成问题，暂不排。
- CR-P3-19 的剩余差距、Linux GUI、Android arm64 真机、BACKLOG 的 P3/P4：同 N 交接 §3.2。

## 4. 批次 U 工作单

| 项 | 步骤 | 验收 |
|---|---|---|
| U0 | 1) `git fetch --prune`；2) 确认 `git merge-base --is-ancestor origin/dev/batch-t-20261121 origin/main` 成立，然后删除这个远端分支；3) 切到 main 并 `git pull --ff-only`，删除本地旧分支，执行 `git worktree prune`；4) 从 main 切出 `dev/batch-u-<日期>`；5) 在 §8 回填 main 上本文所在提交的 CI 运行编号与结论；6) 按 §2.2 核对 Gradle 发行包 | `git branch -a` 只剩 main 和本批分支；`git worktree list` 只剩主目录；两条命令的输出原样贴进 §8；§8 有 CI 回填行和 Gradle 的两个 SHA-256 |
| U1 | 按 §2.3 重建 `libak_ffi.so`，T1 测试跑一轮 | 通过，每个文件的 SHA-256 逐个相等；§8 贴 .so 的构建行、版本行、结论行、每个文件的大小与摘要前 8 位 |
| U2 | 按 §2.4 新增 runner，做开始前的两项记录，再走 L1 至 L4 | 四种情形都有 §2.4 要求的记录，L3、L4 有第二段；做不到的情形写明卡在哪一步、原因 |
| U3 | 按 §2.5 连续运行 t06 二十次 | 20 次都通过；§8 贴 20 行结论 |
| U4 | §5 的全部门槛；U2 新增了 Dart 文件，提交前在 `flutter_app` 下跑一次 `flutter analyze` | 全部通过；失败按 §6 记录 |

并行建议：U3 只用 Windows，U1、U2 要模拟器。同一台机器上，先 U0、U3，再 U1、U2；U4 最后做。

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
- 一项被后续批次确认或推翻时，原地改写它的那一行，不要只在表尾追加新行。改写时保留原有的缺陷描述、修复描述和修复提交号，只补充新的结论。T-N1 一行由 U3 的结论原地补充，T-N2 一行由 U1 的结论原地补充，T-N3 一行由 U0 的 Gradle 核对原地补充。
- run 号和计数只写在本文 §8；STATUS 只写阶段结论、下一步和链接，并链接到本文。改 STATUS 时整份改写，保留全部入口链接。
- 状态表表尾的说明行由复核方在复核时写，执行方不要加。
- 本文不要同时出现“已核”与“尚待 / 待补 / 仍待 / 待执行”（`check_repository.py` 会拦截）。
- 执行方只推 `dev/batch-u-<日期>`；复核通过后由复核方快进合入 main，再由执行方删除分支。任何人都不强推，包括改写自己刚推上去的提交：发现错误就追加一个新提交。
- 新增的 CI 门槛如果失败，先查清原因；不要为了让它通过而削弱它。
- 失败都要记入 §8，不能只记最后一次：
  - CI 运行失败，记运行编号与失败原因；
  - 测试失败后重跑通过，记失败那次的完整输出与原因分析。
- 报完成之前按 §2.1 确认推送与 CI。
- 验收要求原样贴出的输出，每次都要贴，不能写成“与上一次一致”。版本行这类每轮都打印的输出，每轮都贴。
- 没有做的项，在 §8 写明没有做和原因。

## 7. 红线

- 不引入 TURN/relay，服务器不经手文件数据（ADR-001）。
- 不部署、不登录、不修改生产服务器，也不改它的防火墙。CLI 的 `--stun` 默认指向生产服务器，本批一律显式指向本机的 STUN。
- 不生成签名密钥。
- 配对密钥、令牌、用户文件、真实文件名或路径、完整网络诊断都不入库；任务码与配对码不入库，指纹与 job id 只写前 8 位。
- 模拟或同机测试不写成真机验证。

## 8. 证据表（执行方填写）

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
