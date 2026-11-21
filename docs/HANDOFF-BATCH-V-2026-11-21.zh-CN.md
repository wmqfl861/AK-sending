# 交接文档：批次 V（2026-11-21）

> 起点：main 快进到批次 U 复核的最终文档提交，本文随该提交入库。
>
> - 批次 U 的结论见 [U 复核](REVIEW_2026-11-21-batchU.zh-CN.md)；
> - 复核方的命令、运行编号与计数见 [U 交接 §9](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#9-复核方证据2026-11-21)；
> - 执行方从本文 §8 证据表开始记录本批的运行编号与计数；
> - 每项状态只写在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

## 0. 一句话现状

批次 U 通过：

- 本机的 Gradle 发行包就是官方原包；
- Android 接收方向回归通过；
- t06 在 Windows 上连续 20 次通过；
- 后台与生命周期走查在模拟器上做完。

U2 的结论由复核方更正（U-N1）：回到前台后，传输在同一任务里恢复了；几轮没传完，是因为 runner 的观察窗只有 10 分钟。后台超过 QUIC 空闲上限（60 秒）时会怎样、进程是否被冻结，本批确认。

复核方在批次 U 分支上：

- 把 Gradle 的官方校验值写进了 wrapper（T-N3 关闭）；
- 完成了 CR-P2-08 的生产代码拆分：四个大文件拆成子模块，纯移动，有记号级证明；
- 把 Android 的后台传输策略交所有者决定（U-N2）。

批次 V：

- V0：收尾清理，回填 main 的 CI；
- V1：Android 接收方向的每批回归，库从拆分后的代码重建；
- V2：U2 的聚焦复测；
- V3：在 Windows 上确认拆分，并做打包冒烟；
- V4：安装、覆盖升级与卸载走查；
- V5：全部门槛。

## 1. 复核方在批次 U 分支上的改动

| 项 | 内容 | 主要位置 |
|---|---|---|
| T-N3 | wrapper 写入 `distributionSha256Sum`（U0 核对过的官方值），注释写明升级 Gradle 时一起改 | `flutter_app/android/gradle/wrapper/gradle-wrapper.properties` |
| CR-P2-08 | 四个大文件的生产代码拆成子模块，纯移动：<br>• `ak-engine/src/lib.rs` → `handshake`、`manifest`、`sender`、`receiver`、`recv_single`、`recv_multi`；<br>• `ak-ffi/src/task.rs` → `task/` 下的 `stack`、`lease`、`ops`、`runs`、`join_send`、`events`；<br>• `ak-cli/src/main.rs` → `cli`、`args`、`peers`、`jobs`、`output`、`bench`、`pair_jobs`、`receive_cmd`、`send_cmd`、`services`；<br>• `ak-signal/src/server_v2/engine.rs` → `engine/` 下的 `state`、`conn`、`hello`、`invites`、`pairing`、`sessions`。<br>公开路径不变；只加了 `pub(super)`，没有新增或去掉 `pub` | 上述目录 |
| tokcmp | 新增 `items` 与 `split` 两个模式，用来证明拆分是纯移动 | `scripts/tokcmp` |
| 文档 | U 复核；U 交接 §9；状态表（CR-P2-08、T-N3、U2、U-N1、U-N2、main 合并与表尾说明，以及 T-N 行的顺序）；STATUS；支持矩阵的 Android 一行；BACKLOG 的 P0-06 两条 | `docs/` |

拆分唯一可观察的差异：从移出代码打出的日志，target 带上了子模块名，例如 `ak_signal::server_v2::engine::pairing`。

## 2. 接手前必读

### 2.1 报完成之前与分支

沿用 U 交接 §2.1，批次 U 都做到了：

- 最后一次推送之后，先运行 `git ls-remote --heads origin`，确认远端有 `dev/batch-v-<日期>` 且指向你的最后一个提交；
- 等这次 CI 全部跑完再报完成；
- 失败了，先在 §8 记下运行编号、失败的任务与测试、完整报错，再追加一个提交推送，重跑通过也照样记失败那次；
- §8 不留占位；回复里写明最后一次推送的提交号、CI 运行编号与结论；
- 一批合入 main 之后，与它有关的新记录写进下一批的分支与交接。

### 2.2 Android 接收方向的每批回归（V1）

- 先运行 `bash scripts/build_android_ffi.sh`，§8 记下 x86_64 一行。拆分改了模块路径（日志 target、panic 信息），所以库的哈希会与批次 U 不同。
- 再运行 `flutter test integration_test/t1_android_app_receive_test.dart -d <模拟器>`，默认 8 MiB，跑一轮。
- §8 贴出：版本行、结论行、每个文件的大小与 SHA-256 前 8 位。

### 2.3 U2 的聚焦复测（V2）

要回答的问题：

1. 后台与锁屏 30 秒后回来，同一任务能否继续并传完；
2. 后台 120 秒（超过 QUIC 两端各 60 秒的空闲上限）之后，连接是否断开、用户看到什么、重发能否续传；
3. 进程在后台是否被冻结。

runner：

- 在 `u2_android_lifecycle_test.dart` 里加一个 `--dart-define`：`AK_U2_WATCH_MIN`，观察窗的分钟数，默认仍是 10。测试整体的超时（现在是 20 分钟）随之放宽到观察窗加 10 分钟。只改测试文件。
- 本批一律用 `AK_U2_MIB=32` 与 `AK_U2_WATCH_MIN=30`。按批次 U 的速率，32 MiB 约 3 到 4 分钟就能传完，观察窗足够。

开始前记录：

- `adb shell getprop ro.build.version.sdk`；
- `adb shell dumpsys activity settings | grep -i freez`：原样贴出这几行，这是 freezer 的实际设置。

四种情形，每种都从头开始：新任务码、新的宿主接收目录、清掉发送端检查点。

| 情形 | 操作 | 额外记录 |
|---|---|---|
| V2-a 后台 30 秒 | 宿主接收进度过约 20% 时执行 `adb shell input keyevent KEYCODE_HOME`，30 秒后执行 `adb shell am start -n dev.ak.ak_sending_app/.MainActivity` | — |
| V2-b 后台 120 秒 | 同上，120 秒后回前台 | 在后台的第 30 秒和第 90 秒各记一次进程状态（命令见表下） |
| V2-c 锁屏 30 秒 | 约 20% 时执行 `adb shell input keyevent KEYCODE_POWER`，30 秒后执行 `adb shell input keyevent KEYCODE_WAKEUP` 与 `adb shell wm dismiss-keyguard` | — |
| V2-d 锁屏 120 秒 | 同上，120 秒后唤醒 | 同 V2-b |

V2-b、V2-d 记进程状态的命令（原样贴出前 20 行）：

```sh
adb shell dumpsys activity processes | grep -i -E "ak_sending|frozen" | head -20
```

每种情形都要记：

- 执行的命令原文与时间；
- runner 从操作前 10 秒到回来后 30 秒的全部 `PROGRESS` 行，原样贴；
- 同一时段宿主接收端的输出，每行前加上时间，例如经 `while read -r l; do echo "$(date +%T) $l"; done` 管道；
- 最终结果：传完时，接收文件的 SHA-256 与源文件比对；失败时，两端的报错原文，以及 runner 的 `RESULT_UI` 一行（应用界面上显示的内容）；
- 失败的情形再做第二段：保留检查点，对同一个文件用新任务码重发，宿主用同一个 `--id-dir` 与 `--out` 加入，记录是续传还是从头开始，以及最终的 SHA-256；
- 每轮的 `SUMMARY` 行。

边界：

- 这是走查，记录实际行为，不改产品代码；
- 不 root，不改 SELinux 模式，不用 `dumpsys deviceidle force-idle` 一类强制进入省电状态的命令；
- 结论里写明是 debug 构建、是模拟器，不能写成真机验证。

### 2.4 在 Windows 上确认拆分（V3）

- 规模测试：`cargo test --locked --release -p ak-itests --test t13_many_files -- --ignored --nocapture --test-threads=1`，原样贴上三行 `phases:`，并与批次 T 的 T2（T 交接 §8）对照。拆分是纯移动，各段耗时应在同一量级。
- 新旧版本互通：
  - 旧构建取拆分前的 `c40cf90`，照 T 交接 §2.5 的做法，用独立的 worktree 与独立的 target 目录构建 release `ak-cli.exe`；
  - 新构建取本批分支的 release 构建；
  - 运行 `bash scripts/mixed_version_check.sh --old <旧> --new <新>`，原样贴出全部 `[mixed]` 行；
  - 用完删除这个 worktree。
- 打包冒烟：
  - 先 `flutter build windows --release`（在 `flutter_app` 下），再运行 `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\package_windows.ps1 -SkipFlutter:$false`；
  - 用包里的 `ak-cli.exe` 跑 `bash scripts/smoke_loopback.sh --binary <包里的 ak-cli.exe> --skip-probe`；
  - §8 写包内文件清单（名称与大小）、版本行、冒烟结论行。

### 2.5 安装、覆盖升级与卸载（V4）

Windows 包：

- 把 V3 打出的 zip 解压到一个新的临时目录；
- 启动包里的桌面应用一次：出现主界面后关闭，再运行一次 `ak-cli.exe --version`；
- 记录这次运行在包目录以外写了哪些目录（只写目录位置的类别，例如“用户主目录下的 `.ak-sending`”，路径按红线略写）；
- 删除解压目录，记录这些目录是否还在。这是将来写“卸载说明”的依据。

Android：

- 旧 APK 用拆分前的 `c40cf90` 在本机构建：在独立 worktree 里运行 `bash scripts/build_android_ffi.sh`（独立的 target 目录），再 `flutter build apk --debug`。必须本机构建：debug APK 用本机的 debug 签名，CI 的 APK 签名不同，不能覆盖安装。
- 新 APK 用本批分支在本机构建。
- 步骤：
  1. `adb uninstall dev.ak.ak_sending_app`，回到干净状态；
  2. `adb install <旧 APK>`，`am start` 启动，等主界面出现；
  3. 用 `adb shell run-as dev.ak.ak_sending_app ls -laR files` 列出应用数据（只看文件名、大小、修改时间，不读内容）；
  4. `adb install -r <新 APK>` 覆盖安装，启动；
  5. 再列一次应用数据，确认身份文件仍在，大小与修改时间不变；
  6. `adb logcat -d -b crash` 没有本应用的崩溃；
  7. `adb uninstall dev.ak.ak_sending_app`，`adb shell pm list packages | grep ak_sending` 为空。
- §8 原样贴出每一步的命令与输出。

### 2.6 仓库检查

`python scripts/check_repository.py` 的规则同 [S 交接 §2.4](HANDOFF-BATCH-S-2026-11-20.zh-CN.md#24-仓库检查)：

- STATUS 要链接 docs 下全部交接与复核文档，以及入口文档；
- 状态表批次行（编号形如 V0、V1、V-N1）的“状态”“证据摘要”两列，以及 STATUS“最新验证”以外的部分，不写运行编号、通过比、测试计数、倍数和带小数或区间的耗时。这些数字只写在本文 §8。

## 3. 全项目未完成事项

### 3.1 所有者事项（agent 不得代办，决定正式完成的时间）

- U-N2：Android 的后台传输策略。A 是 v1 只承诺前台传输，加界面提示与文档；B 是 v1 加前台服务。见 [U 复核 §2](REVIEW_2026-11-21-batchU.zh-CN.md#u-n2p2产品决定android-的后台传输策略)。
- P2-02：v1 是否需要移动数据授权。
- E6 生产部署、C4 物理双机跨 NAT、E5 签名、E7 许可证、CR-P3-03、CR-P3-05，以及 P4-02 的外部安全审查，见 [状态表 §8](REVIEW_2026-11-11.zh-CN.md#8-需所有者确认的事项)。M2 与之后各轮的改动，要等 E6 部署后才进入生产服务器。CR-P1-02/05/06 的真机验收、P0-05 的网络样本与 P0-07 的抓包都依赖 C4；C4 里的 Android 手机要再确认一次接收方向（S-N3）与后台行为（U-N2）。

### 3.2 工程项

- 复核方，V 复核时：
  - 按所有者对 U-N2 的决定处理 Android 的后台策略；
  - 写发布说明；
  - 刷新 ROADMAP、支持矩阵与 BACKLOG。
- [BACKLOG](BACKLOG.zh-CN.md) P3-02 之下仍开着块阶段的检查点保存一项，默认时限下不构成问题，暂不排。
- CR-P3-19 的剩余差距、Linux GUI、Android arm64 真机、BACKLOG 的 P3/P4：同 N 交接 §3.2。

## 4. 批次 V 工作单

| 项 | 步骤 | 验收 |
|---|---|---|
| V0 | 1) `git fetch --prune`；2) 确认 `git merge-base --is-ancestor origin/dev/batch-u-20261121 origin/main` 成立，然后删除这个远端分支；3) 切到 main 并 `git pull --ff-only`，删除本地旧分支，执行 `git worktree prune`；4) 从 main 切出 `dev/batch-v-<日期>`；5) 在 §8 回填 main 上本文所在提交的 CI 运行编号与结论 | `git branch -a` 只剩 main 和本批分支；`git worktree list` 只剩主目录；两条命令的输出原样贴进 §8；§8 有回填行 |
| V1 | 按 §2.2 重建 `libak_ffi.so`，T1 测试跑一轮 | 通过，每个文件的 SHA-256 逐个相等；§8 贴 .so 的构建行、版本行、结论行、每个文件的大小与摘要前 8 位 |
| V2 | 按 §2.3 改 runner，做开始前的两项记录，再走 V2-a 至 V2-d | 四种情形都有 §2.3 要求的记录，失败的情形有第二段；做不到的情形写明卡在哪一步、原因 |
| V3 | 按 §2.4 运行规模测试、新旧版本互通与打包冒烟 | 规模测试三项通过，§8 贴三行 `phases:` 与对照；互通八项全部通过，§8 贴全部 `[mixed]` 行与两个版本行；打包成功、冒烟通过 |
| V4 | 按 §2.5 走 Windows 包与 Android 的安装、覆盖升级、卸载 | §8 原样贴出每一步的命令与输出；覆盖升级后身份文件仍在，应用没有崩溃 |
| V5 | §5 的全部门槛；V2 改了 Dart 文件，提交前在 `flutter_app` 下跑一次 `flutter analyze` | 全部通过；失败按 §6 记录 |

并行建议：V3 与 V4 的 Windows 部分只用 Windows，V1、V2 与 V4 的 Android 部分要模拟器。同一台机器上，先 V0、V3（规模测试占磁盘和 CPU），再 V1、V2、V4，V5 最后做。

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
- 新增的行放在本批前一批各行之后：先 V0–V5，复核方的 V-N 行由复核方加。
- 一项被后续批次确认或推翻时，原地改写它的那一行，不要只在表尾追加新行。改写时保留原有的缺陷描述、修复描述和修复提交号，只补充新的结论。CR-P2-08 一行由 V1、V3 的结论原地补充，U-N1 一行由 V2 的结论原地补充。
- run 号和计数只写在本文 §8；STATUS 只写阶段结论、下一步和链接，并链接到本文。改 STATUS 时整份改写，保留全部入口链接。
- 状态表表尾的说明行由复核方在复核时写，执行方不要加。
- 本文不要同时出现“已核”与“尚待 / 待补 / 仍待 / 待执行”（`check_repository.py` 会拦截）。
- 执行方只推 `dev/batch-v-<日期>`；复核通过后由复核方快进合入 main，再由执行方删除分支。任何人都不强推，包括改写自己刚推上去的提交：发现错误就追加一个新提交。
- 新增的 CI 门槛如果失败，先查清原因；不要为了让它通过而削弱它。
- 失败都要记入 §8，不能只记最后一次：
  - CI 运行失败，记运行编号与失败原因；
  - 测试失败后重跑通过，记失败那次的完整输出与原因分析。
- 报完成之前按 §2.1 确认推送与 CI。
- 验收要求原样贴出的输出，每次都要贴，不能写成“与上一次一致”；版本行这类每轮都打印的输出，每轮都贴。
- 结论要与同一行的数字一致：写“没有恢复”之前，先对照进度数字与最长停顿（U-N1 的教训）。
- 没有做的项，在 §8 写明没有做和原因。

## 7. 红线

- 不引入 TURN/relay，服务器不经手文件数据（ADR-001）。
- 不部署、不登录、不修改生产服务器，也不改它的防火墙。CLI 的 `--stun` 默认指向生产服务器，本批一律显式指向本机的 STUN。
- 不生成签名密钥。V4 用的是本机已有的 debug 签名，不新建 release 签名。
- 配对密钥、令牌、用户文件、真实文件名或路径、完整网络诊断都不入库；任务码与配对码不入库，指纹与 job id 只写前 8 位；V4 只列文件名、大小与时间，不读身份文件的内容。
- 模拟或同机测试不写成真机验证。

## 8. 证据表（执行方填写）

执行方在本机 Windows 上工作（rustc 1.99.1）。V2/V4 的模拟器是本机 AVD `ak_test`（Android 15，API 35，x86_64）——模拟器记录不写成真机验证；任务码与配对码不入库，指纹与 job id 只写前 8 位。

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
| main 上批次 U 最终提交的 CI | `328d06d` run [44907999725](https://github.com/wmqfl861/AK-sending/actions/runs/44907999725) | completed success（12 项常规任务 success，2 项手动深测 skipped），以 API 核实 |
| V0 分支与 worktree | 本地 Windows | `git merge-base --is-ancestor origin/dev/batch-u-20261121 origin/main` 成立后删除了该远端分支；本地 main 快进到 `328d06d`，删除本地 `dev/batch-u-20261121`，`git worktree prune` 后从 main 切出 `dev/batch-v-20261121`。`git branch -a` 原样输出：`* dev/batch-v-20261121`、`  main`、`  remotes/origin/HEAD -> origin/main`、`  remotes/origin/main`。`git worktree list` 原样输出（V3/V4 的旧版本 worktree 建于本项之后，用完即删；本行以创建本批分支时为准，路径按红线略写）：`<repo> 328d06d [dev/batch-v-20261121]`，只有主目录一行 |
| V1 FFI 重建 | 本地 Windows，`bash scripts/build_android_ffi.sh` | 成功。脚本输出原样：`==> x86_64    libak_ffi.so     9698152 bytes  sha256 4e8a0590b3688983a720587bf86cd55168638f2f1edcad04fcfbe6e25a9f4cc6`——哈希与批次 U 的 d5b06e07… 不同、大小 +32 字节，正是拆分改变模块路径的预期表现（§2.2） |
| V1 T1 回归（8 MiB，模拟器） | 模拟器（应用进程内回环） | **PASS**（`All tests passed!`，退出码 0）。`engine.version(): ak-ffi 1 (wire v1, caps 0x0017)`；接收端指纹 32199ce7；`both jobs done: files=6 bytes=8388663`；`payload.bin: 8388608 bytes, sha256(8) a37b426c`；`说明-中文文件名.txt: 40 bytes, sha256(8) 5abef6f0`；`nested/level2/inner.txt: 15 bytes, sha256(8) 4e7267fd`；`PASS: 8 MiB round — SHA-256 equal per file, layout equal`。拆分后的库在应用域的接收路径完好 |
| V3 规模测试（拆分确认） | 本地 Windows（797.46s，三项全过） | `T13-20000: phases: manifest received @ 7.9s, first chunk sent @ 23.1s, last chunk received @ 34.4s, verify phase started @ 36.6s, final ack @ 61.2s, final ack @ 61.2s, done @ 61.2s, done @ 61.2s`，校验 36.6→61.2 = 24.6s（T2 同机 27.1s，同量级）；`T13-65537: phases: manifest received @ 24.0s, first chunk sent @ 61.0s, last chunk received @ 101.7s, verify phase started @ 109.4s, final ack @ 209.3s, final ack @ 209.3s, done @ 209.3s, done @ 209.4s`，校验 109.4→209.3 = 99.9s（T2 93.6s，同量级）；`T13-65537-idle4s: phases: manifest received @ 23.8s, first chunk sent @ 63.1s, last chunk received @ 103.5s, verify phase started @ 111.2s, final ack @ 204.6s, final ack @ 204.6s, done @ 204.7s, done @ 204.7s`，last chunk 到 verify 的间隔 103.5→111.2 = 7.7s（T2 8.3s，同量级）。三行耗时与拆分前同量级，纯移动的结论与 Windows 上的实测一致 |
| V3 新旧版本互通 | 本地 Windows（旧构建：c40cf90 独立 worktree + 独立 target） | `[mixed] old: ak-cli 0.1.0 (wire v1, caps 0x0017 [resume|pause-resume|multistream|sender-keepalive], release build, git c40cf90)`；`[mixed] new: ak-cli 0.1.0 (wire v1, caps 0x0017 [resume|pause-resume|multistream|sender-keepalive], release build, git 328d06d-dirty)`（dirty 源于构建时工作区含本批未提交的 runner 改动，已如实记录）；`[mixed] new->new single-stream: PASS (job 2bf6fbdf, tree identical)`；`[mixed] new->new --multistream: PASS (job 2bf6fbdf, tree identical)`；`[mixed] new->old single-stream: PASS (job 2bf6fbdf, tree identical)`；`[mixed] new->old --multistream: PASS (job 2bf6fbdf, tree identical)`；`[mixed] old->new single-stream: PASS (job 2bf6fbdf, tree identical)`；`[mixed] old->new --multistream: PASS (job 2bf6fbdf, tree identical)`；`[mixed] old->old single-stream: PASS (job 2bf6fbdf, tree identical)`；`[mixed] old->old --multistream: PASS (job 2bf6fbdf, tree identical)`；`[mixed] RESULT: PASS (8/8 runs)` |
| V3 打包冒烟 | 本地 Windows | `flutter build windows --release` 成功后 `package_windows.ps1 '-SkipFlutter:$false'` 成功（首次调用时 Git Bash 把 `$false` 展开成空串、Flutter 未进包，已如实记录并加引号重跑）。包内 21 个文件共 61,767,538 字节（较批次 S 多出的 14 个是 flutter\ 子目录：ak_sending_app.exe、flutter_windows.dll、dartjni.dll、ak_ffi.dll、data\ 下 app.so、icudtl.dat、flutter_assets 等）；`flutter\ak_ffi.dll unified with target\release\ak_ffi.dll (SHA256 … verified on both copies)`；zip 9,339,629 字节。包里 `ak-cli.exe --version` 原样：`ak-cli 0.1.0 (wire v1, caps 0x0017 [resume|pause-resume|multistream|sender-keepalive], release build, git 328d06d-dirty)`；冒烟结论行原样：`[smoke] 4/4 files identical (cmp), empty dir present, 4 files on disk`、`[smoke] pair books symmetric (A explicit, B TOFU); both checkpoints completed`、`[smoke] PASS: real two-process loopback smoke completed (port 17777)` |
| V2 前置记录 | 模拟器（开始前） | `getprop ro.build.version.sdk` = `35`；`dumpsys activity settings | grep -i freez` 原样（节选）：`use_freezer=true`、`freeze_debounce_timeout=10000`、`freeze_binder_enabled=true`、`freeze_exempt_inst_pkg=false`、`freeze_statsd_sample_rate=0.1`——ActivityManager 层 freezer 已启用（去抖 10 秒）；批次 U 记录的 `settings get global cached_apps_freezer` = null 是另一个开关，两者不矛盾，本批以 dumpsys 为准 |
| V2-a 后台 30 秒 | 模拟器 + 宿主（32 MiB，速率约 0.2 MiB/s；观察窗 30 分钟） | 传输中（24.0 MiB / 71.9%）时 `KEYCODE_HOME`（t=224.6s），31.4 秒后 `am start` 回前台（t=253.8s）。采样窗原样（节选自脚本输出，全量在 /tmp 证据）：t=218.0s bytes=23068672（life=resumed）→ t=224.6s 24117248 → t=231.9s 25165824（life=paused）→ t=235.2s–253.8s 恒为 26214400（75.0%→78.1%，sinceAdvance 增至 20 秒）——后台期间先前进约 1 MiB 后冻结。回前台后**同一任务恢复传输并传完**：SUMMARY 原样要点 `ended=completed|sendCompleted=true|bgAdvanceBytes=1048576|maxStallMs=41488|lifecycleResumedAgain=true`；RESULT_UI 原样 `发送完成：已发送 1 个文件（32.0 MB） · 互联网直连 / 用时 4 分 15 秒 · 平均 128 KB/s`；接收文件 SHA-256 与源逐字相等（424900a2…e035246）。**结论与数字一致：30 秒后台（冻结约 20-30 秒）回前台后同任务恢复并完成** |
| V2-b 后台 120 秒，第一段 | 同上 | 传输中 `KEYCODE_HOME`，123.0 秒后回前台。宿主侧在后台期间即报错：`Error: the peer disconnected during the transfer`（最后确认 16.00 MiB，后台第 90 秒采样时已断）——**超过 QUIC 60 秒空闲上限，连接在后台期间断开**。两次 `dumpsys activity processes` 采样原样（节选）：第 30 秒 `isFrozen=true`、第 90 秒 `isFrozen=true`（isFreezeExempt=false）。回前台后：**应用窗口纯黑**（亮度均值 0.6，am start 置顶无效；进程存活、runner 仍在打 PROGRESS）——首帧表面未恢复；观察窗内连接行一直在列表里无错误无结束标记（RESULT_UI 原样 `ok=|error=|endedPill=false`，ended=row-gone 到观察窗结束才触发）。**用户看到什么：黑屏；恢复后界面显示一条看似还在传输、实际已死的行** |
| V2-b 第二段（保留检查点） | 同上（同一文件、新任务码、同 `--id-dir` 与 `--out`） | 宿主原样 `[recv] resuming after 17 verified chunks`；runner `resumeSuspect=true`（首个非零进度即续传位置）；**传完且 SHA-256 与源逐字相等**。结论与数字一致：断开后用新任务码重发是续传，不是从头 |
| V2-c 锁屏 30 秒 | 同上 | 传输中 `KEYCODE_POWER` 灭屏，30 秒后 `KEYCODE_WAKEUP` + `wm dismiss-keyguard`。**同一任务恢复传输并传完**：`SHA-256 相等：u2-payload.bin（33554432 字节）`，SUMMARY `sendCompleted=true`，无需第二段。与 V2-a 同型：短时锁屏（冻结）回前台后同任务恢复 |
| V2-d 锁屏 120 秒，第一段 | 同上 | 灭屏 123 秒。两次 dumpsys 采样：第 30 秒 `isFrozen=true`、第 90 秒 `isFrozen=true`，唤醒后 `isFrozen=false`。宿主侧 `Error: the peer disconnected during the transfer`；回前台后界面这次**有明确报错**：RESULT_UI 原样 `error=发送失败：AkFfiException(AK_FAILED 2): transfer failed after connecting via …: stream failed: connection lost: connection lost|role=配对已闲置过久，请重新配对。`（与 V2-b 的僵尸行不同：本情形 UI 在观察窗内报出了失败） |
| V2-d 第二段（保留检查点） | 同上 | 宿主原样 `[recv] resuming after 17 verified chunks`；**传完且 SHA-256 与源逐字相等**。与 V2-b 第二段同型 |
| V2 结论与过程记录 | 模拟器（debug 构建）+ 宿主同机 | 四情形回答了 §2.3 的三个问题：(1) **后台/锁屏 30 秒（含冻结）回前台后，同一任务恢复并传完**（V2-a、V2-c，SHA 相等）；(2) **120 秒（超过 QUIC 60 秒空闲上限）连接在后台/锁屏期间断开**（宿主 `peer disconnected`），UI 表现两次不同（V2-b 黑屏+僵尸行；V2-d 明确报错文案），**新任务码+保留检查点重发为续传**（两段都是 17 verified chunks 起）并传完、SHA 相等；(3) **进程在后台确被冻结**（dumpsys 两次采样 isFrozen=true，唤醒后解冻）。过程记录：V2a 首次运行 home 150 秒未渲染而失败——A/B 判别（拆分前 .so 与拆分后 .so 各跑一轮无宿主对照）两种 .so 都正常渲染并建码，判为偶发；两次失败的场景启动是 /tmp 脚本的函数先序 bug 与一次 registry-id 瞬时失败（已修复，非仓库问题）。走查不改产品代码；黑屏与僵尸行交复核方 |
| V4 Windows 包 | 本地 Windows（子代理执行，记录全文在 /tmp/ak-v4-win/record.md） | 解压 zip 到新临时目录；桌面应用启动**主窗口 8.6 秒出现**（标题 `ak_sending_app`），CloseMainWindow 干净退出；`ak-cli.exe --version` 原样 `ak-cli 0.1.0 (wire v1, caps 0x0017 [resume|pause-resume|multistream|sender-keepalive], release build, git 328d06d-dirty)`；包外新写**仅 `%TEMP%\ak-stage.log`（201 B，应用启动阶段日志）**——`~/.ak-sending`、%APPDATA%、%LOCALAPPDATA% 本次零改动（本机既有的 `.ak-sending` 与 `dev.ak\ak_sending_app` 为此前数据，按红线甄别为既有）；删除解压目录后 ak-stage.log 仍在（卸载需手删），无残留进程 |
| V4 Android 覆盖升级 | 模拟器（本机 debug 签名，两 APK 证书 SHA-256 相同 1df81fe0…，apksigner verify 通过） | 序列原样要点：`adb uninstall` 时包本就不在（报 DELETE_FAILED_INTERNAL_ERROR，包计数 0，视为干净）；装旧 APK（c40cf90 构建）`Success`，`am start` 后**约 30-75 秒主界面才渲染**（冷启动首帧很慢，debug 构建；两次实测 14s 时亮度 11.2（闪屏）、30s/75s 时 40.1（主界面））；旧版数据目录：`files/` 仅 `profileInstalled`（24 B）——**冷启动不创建身份文件**（首次任务/配对才建），shared_prefs 为空，`cache/ak-stage.log`（402 B）；执行方在 files/ 放标记文件 `v4-marker.txt`（21 B，内容 v4-marker-<epoch>）作为保留性证据；`adb install -r` 新 APK `Success`，启动主界面同样约 30 秒渲染；升级后数据：`v4-marker.txt` **内容逐字保留**、目录结构一致、`profileInstalled` 刷新；崩溃缓冲仅 1 条且是系统蓝牙进程（09:55，与本应用无关）——**应用无崩溃**；`adb uninstall` `Success`，`pm list packages | grep ak_sending` 计数 0。边界：身份文件保留性检查因冷启动不建身份而降级为标记文件检查，如实记录 |
| V5 全量门槛 | 本地 Windows（rustc 1.99.1） | fmt/clippy/rustdoc/workspace（69 组，1231/0/11）/sqlite（54/0）/server-persistence（221/0）/Python（372 OK）/repo/contracts（5/5 PASS）/部署自检（PASS）/跨NAT自检（PASS）全部通过；`flutter analyze`（flutter_app 下，含本批改过的 u2 runner）`No issues found!`；`check_repository.py` 在回填本表提交号后通过（回填前只报状态表提交列占位） |
| V 批次最终 CI | `e65fccc` run [44926202524](https://github.com/wmqfl861/AK-sending/actions/runs/44926202524) | **completed success**（12 项常规任务 success，2 项手动深测 skipped） |
