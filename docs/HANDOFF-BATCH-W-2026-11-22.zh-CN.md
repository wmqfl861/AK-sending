# 交接文档：批次 W（2026-11-22）

> 起点：main 快进到批次 V 复核的最终文档提交，本文随该提交入库。
>
> - 批次 V 的结论见 [V 复核](REVIEW_2026-11-22-batchV.zh-CN.md)；
> - 全项目审核与界面设计方案见 [全项目审核](AUDIT_2026-11-22.zh-CN.md)；
> - 复核方的命令与运行编号见 [V 交接 §9](HANDOFF-BATCH-V-2026-11-21.zh-CN.md#9-复核方证据2026-11-22)；
> - 执行方从本文 §8 证据表开始记录本批的运行编号与计数；
> - 每项状态只写在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

## 0. 一句话现状

批次 V 通过：拆分在 Windows 与模拟器两侧确认无损；U2 的更正被实测证实；后台约 30 秒能自动恢复，超过约 60 秒连接断开、重发可续传。

V2 同时暴露了三个产品问题：

- V-N2：长时间后台后，连接已断，界面却一直显示在传输（僵尸行）；
- V-N3：一次回前台后窗口全黑；
- V-N4：错误显示异常原文，“配对已闲置过久”的文案与真实原因不符。

本次复核还做了全项目审核。结论是核心工程扎实，接下来要把重心转到产品体验。**批次 W 是第一个以修产品问题为主的批次**：

- W0：收尾清理；
- W1：修僵尸连接行（V-N2，P1）；
- W2：复现并处理黑屏（V-N3）；
- W3：错误文案映射（V-N4）；
- W4：应用名、窗口标题与模板残留；
- W5：打包卫生（V-N6）；
- W6：性能实测（V-N5）；
- W7：三处交互小改；
- W8：归档已关闭批次的文档（可选）；
- W9：全部门槛与回归走查。

## 1. 复核方在批次 V 分支上的改动

| 项 | 内容 | 主要位置 |
|---|---|---|
| V-N1 | 恢复 V 交接 §8 被 shell 转义破坏的一行与三处路径；`check_repository.py` 新增规则，Markdown 里出现 TAB、LF 以外的 C0 控制字符（含 CR）即报错；三条单元测试 | `docs/HANDOFF-BATCH-V-2026-11-21.zh-CN.md`、`scripts/check_repository.py`、`scripts/tests/test_repository_checks.py` |
| 审核 | 全项目审核与界面设计方案 | `docs/AUDIT_2026-11-22.zh-CN.md` |
| 文档 | V 复核；V 交接 §9；状态表（V-N1 至 V-N6、main 合并一行、表尾说明）；STATUS；本文 | `docs/` |

没有改产品代码。

## 2. 接手前必读

### 2.1 报完成之前与分支

沿用 V 交接 §2.1：

- 最后一次推送之后，运行 `git ls-remote --heads origin`，确认远端有 `dev/batch-w-<日期>` 且指向你的最后一个提交；
- 等这次 CI 全部跑完再报完成；
- 失败了，先在 §8 记下运行编号、失败的任务与测试、完整报错，再追加一个提交推送，重跑通过也照样记失败那次；
- §8 不留占位；回复里写明最后一次推送的提交号、CI 运行编号与结论。

### 2.2 往文档里写内容（V-N1 的教训）

- 写含反斜杠的内容（Windows 路径）时，用编辑器，或用带引号的 heredoc（`cat <<'EOF'`）；不要用 `echo -e`、不带引号的 heredoc 或 `printf` 的格式串。
- `check_repository.py` 现在会拦下 Markdown 里的控制字符；它报错时先看是不是转义问题。

### 2.3 本批会改产品代码

与前几批不同，W1、W3、W4、W5、W7 会改 Rust、Dart 与平台工程。要求：

- 每个修复都要有能在 CI 跑的自动化测试（Rust 单元或集成测试、Dart 单元测试、Flutter widget 测试），只有走查结果的修复不算完成；
- 改 FFI 的 JSON 字段或事件时先改 `docs/contracts/ffi-v2.md`，保持纯加法，并跑 `python scripts/check_contracts.py`；
- 不改线上协议（`ak-wire`、`protocol-v2.md`）；需要改协议时停下，在 §8 写明原因，交复核方；
- 不新增第三方依赖（ADR-002）。Flutter SDK 自带的包（如 `flutter_localizations`）不算第三方，但本批也不需要；
- 改动的现有测试断言（例如断言了旧文案）在 §8 列出：哪个测试、旧断言、新断言、为什么。

### 2.4 Android 走查的共同做法

沿用 V 交接 §2.3 的 runner、命令与边界（不 root、不改 SELinux、不强制省电状态）：

- 每种情形从头开始：新任务码、新的宿主接收目录、清掉发送端检查点；
- 记录执行的命令原文与时间、操作前 10 秒到回来后 30 秒的 `PROGRESS` 行、宿主带时间戳的输出、`RESULT_UI` 与 `SUMMARY` 行；
- 回前台后 5 秒与 30 秒各截一次屏（`adb exec-out screencap -p > <文件>`），§8 只写画面是否正常与亮度均值，截图本身不入库；
- 结论写明是 debug 或 profile 构建、是模拟器。

## 3. 全项目未完成事项

### 3.1 所有者事项（agent 不得代办）

见 [审核 §6](AUDIT_2026-11-22.zh-CN.md#6-需要所有者决定的事项)：

- O-1（U-N2）Android 后台策略：复核方建议 A；
- O-2（P2-02）移动数据；
- O-3 应用显示名：W4 先按“AK互传”实施，所有者可改；
- O-4 应用图标与 `applicationId`：本批不改；
- O-5 v1 平台范围；
- O-6 新依赖审批（二维码显示与扫码，供批次 X 使用）；
- O-7 流程瘦身；
- E5 签名、E6 部署、E7 许可证、C4 物理双机，见 [状态表 §8](REVIEW_2026-11-11.zh-CN.md#8-需所有者确认的事项)。

### 3.2 工程项

- 本批之后的界面改造（新首页与两条主路径、二维码、接收确认显示内容摘要、拖放、分享入口、通知、`gen-l10n`、Windows 与 release APK 进 CI）属于批次 X，见 [审核 §5](AUDIT_2026-11-22.zh-CN.md#5-分阶段路线)；
- PAKE 短码与局域网“附近设备”属于批次 Y，需要先写 ADR-003；
- 其余沿用 V 交接 §3.2。

## 4. 批次 W 工作单

| 项 | 步骤 | 验收 |
|---|---|---|
| W0 | 1) `git fetch --prune`；2) 确认 `git merge-base --is-ancestor origin/dev/batch-v-20261121 origin/main` 成立，然后删除这个远端分支；3) 切到 main 并 `git pull --ff-only`，删除本地旧分支，执行 `git worktree prune`；4) 从 main 切出 `dev/batch-w-<日期>`；5) 在 §8 回填 main 上本文所在提交的 CI 运行编号与结论 | `git branch -a` 只剩 main 和本批分支；`git worktree list` 只剩主目录；两条命令的输出原样贴进 §8；§8 有回填行 |
| W1 | 修 V-N2 僵尸连接行，按 §4.1 | §4.1 的四条全部满足 |
| W2 | 复现并处理 V-N3 黑屏，按 §4.2 | 复现了：修复后三轮都正常；没复现：按 §4.2 的轮数记录后关闭 |
| W3 | 错误文案映射，按 §4.3 | §4.3 的四条全部满足 |
| W4 | 应用名与模板残留，按 §4.4 | 三个平台显示“AK互传”；构建、`flutter analyze`、Flutter 测试通过 |
| W5 | 打包卫生，按 §4.5 | 干净工作区打包成功；脏工作区默认失败、`-AllowDirty` 放行并写入清单；启动日志不再写进 `%TEMP%` |
| W6 | 性能实测，按 §4.6 | §8 有 profile 构建冷启动到首帧的三次数字；超过 3 秒时附初步定位 |
| W7 | 三处交互小改，按 §4.7 | widget 测试覆盖三处改动；V2-a 走查一轮通过且传输期间屏幕常亮 |
| W8 | 可选：归档已关闭批次的文档，按 §4.8 | `check_repository.py` 通过；STATUS 的链接明显变短 |
| W9 | §5 的全部门槛；W1 修复后的 V2-b 两轮、V2-d 一轮；W7 后的 V2-a 一轮；T1 回归一轮 | 全部通过；走查结论写在 §8 |

### 4.1 W1：僵尸连接行（V-N2）

背景：V2-b 与 V2-d 都是后台约 120 秒、连接在后台断开，但 V2-b 回来后连接行一直显示在传输，V2-d 回来后报出了失败。V2-d 显示的“配对已闲置过久”来自引擎的 `idle_expired` 原因，而这次后台只有约 120 秒，远短于 10 分钟的配对闲置预算，所以原因判断也可疑。

步骤：

1. **找根因。** 用 `u2_android_lifecycle_test.dart` 按 V2-b 复现（`AK_U2_MIB=32`、`AK_U2_WATCH_MIN=15`），打开引擎日志。对照同一次运行里：引擎有没有为这个连接发出终止事件（`disconnected`、`failed` 或任务的 `done`/错误）；Dart 侧（`lib/services/simple_transfer_service.dart` 的 `_markTerminal` 与事件路由、`TransferServiceLifecycleObserver` 回前台时的 `_pumpOnce`）有没有处理它；连接快照里这一行的状态。再按 V2-d 跑一轮，找出两者分叉的地方，并查清 `idle_expired` 为什么会出现。
2. **在丢失的那一层修。** 原则：
   - 引擎侧：任何一个 QUIC 连接关闭（包括对端消失、空闲超时），对应的连接都必须有且只有一个终止事件，原因如实（连接中断与闲置过期分开）；
   - 应用侧：回到前台时与引擎的连接快照核对一次，引擎已不认识或已终止的连接立即标记为失败；进行中的传输超过发送端停滞预算（150 秒）没有任何进度时，也标记为失败。
   两侧都缺就两侧都修。
3. **自动化测试。**
   - Rust：对端端点直接丢弃、不发关闭帧，断言本端在空闲上限加余量之内发出终止事件，原因是连接中断而不是闲置过期（可用测试专用的短空闲配置，不改生产常量）；
   - Dart：假引擎在应用后台期间悄悄丢掉连接、不发事件，回前台后一次 pump 之内该行变为失败；
   - 若 `idle_expired` 是误判，加一条测试钉住正确的原因。
4. **走查确认（放在 W9）。** V2-b 两轮、V2-d 一轮：回前台后 10 秒内界面显示失败与“重新发送”（文案按 W3），之后重发续传并传完，SHA-256 相等。

验收：

- §8 写明根因：在哪一层、V2-b 与 V2-d 为什么不同、`idle_expired` 是怎么来的；
- 上面的测试加入并通过，CI 绿；
- 走查三轮都符合第 4 步；
- 没有改线上协议。

### 4.2 W2：回前台黑屏（V-N3）

1. 用 profile 构建跑 V2-b 三轮（`flutter drive --profile --driver=test_driver/integration_test.dart --target=integration_test/u2_android_lifecycle_test.dart`，没有 `test_driver/integration_test.dart` 就按 Flutter 官方模板新增这个文件），每轮回前台后 5 秒、30 秒截屏；再用 debug 构建跑两轮。可与 W1 第 1 步合并运行，节省时间。
2. 五轮都正常：§8 记录每轮的亮度均值，V-N3 记为“未复现，关闭”。
3. 任何一轮黑屏：
   - 先试 `--no-enable-impeller`（或 `AndroidManifest` 的 `io.flutter.embedding.android.EnableImpeller=false`）对照一轮，判断是不是渲染后端的问题；
   - 修复方向：回前台时（`AppLifecycleState.resumed`）调用 `WidgetsBinding.instance.scheduleForcedFrame()`；必要时在 `MainActivity` 的 `onResume` 里请求重绘；
   - 修复后 profile 三轮都正常才算完成；
   - 修复代码要有测试（至少一条 widget 测试，断言回前台时请求了新帧）。

### 4.3 W3：错误文案映射（V-N4）

1. 新建 `flutter_app/lib/ui/service/transfer_errors.dart`，一个纯函数，把错误对象（含 `AkFfiException` 的错误码与原文）和场景（配对、发送、接收、导出诊断）映射成四项：标题、原因一句、建议一句、详情原文。错误码以 `docs/contracts/ffi-v2.md` 的错误码表为准。
2. 至少覆盖 [审核 §4.3](AUDIT_2026-11-22.zh-CN.md#43-关键流程) “失败与恢复”表里的五种情况，外加“未知错误”兜底（标题“出了点问题”，建议“重试；仍不行请导出连接诊断”）。文案遵守 [审核 §4.5](AUDIT_2026-11-22.zh-CN.md#45-文案规范)。
3. 替换界面里所有直接显示错误原文的地方：连接详情页的“发送失败：…”、接收横幅的“接收失败：…”、首页配对流程的失败视图、设置页的导出失败。原文只出现在可展开的“详情”里，并带复制按钮。
4. 修正 `connection_detail_page.dart` 里“配对已闲置过久，请重新配对。”的显示条件：只在引擎给出闲置过期原因时显示；连接中断显示 W1 定义的中断文案。与 W1 的根因结论保持一致。

验收：

- 映射函数有表驱动的单元测试，每种情况一条，外加兜底；
- widget 测试：失败卡片显示标题与建议；不展开时界面上找不到 `AkFfiException`、`AK_FAILED` 字样；展开后能看到原文；
- 现有测试中断言旧文案的，按 §2.3 在 §8 列出改动；
- 现有的 `ValueKey` 保留，避免无关测试失效。

### 4.4 W4：应用名与模板残留

按所有者事项 O-3 的建议，显示名用“AK互传”。只改显示用的名字，**不改** `applicationId`、可执行文件名 `ak_sending_app.exe`、Dart 包名（O-4）。

| 位置 | 改成 |
|---|---|
| `flutter_app/android/app/src/main/AndroidManifest.xml` 的 `android:label` | `AK互传` |
| `flutter_app/windows/runner/main.cpp` 的窗口标题 | `L"AK互传"` |
| `flutter_app/windows/runner/Runner.rc` 的 `FileDescription`、`ProductName` | `AK互传`（文件已有 `#pragma code_page(65001)`，保存为 UTF-8）；`InternalName`、`OriginalFilename` 不动 |
| `flutter_app/linux/runner/my_application.cc` 两处标题 | `AK互传` |
| `flutter_app/lib/ui/app.dart` 的 `MaterialApp.title` | `AK互传` |
| `flutter_app/pubspec.yaml` 的 `description` | `AK互传：不经服务器的跨设备原文件直传。` |

验收：Windows 包启动后窗口标题是“AK互传”（§8 写 `Get-Process` 的 `MainWindowTitle`）；Android 用 `aapt2 dump badging <apk>` 或 `apkanalyzer manifest print` 看到新标签；`flutter analyze` 与全部 Flutter 测试通过。若有测试断言了旧标题，按 §2.3 记录。

### 4.5 W5：打包卫生（V-N6）

1. 启动阶段日志（`lib/services/ak_engine_facade.dart` 的 `ak-stage.log`）改写到应用数据目录下的 `logs/`：Windows 与 Linux 用身份目录（`.ak-sending`）下的 `logs/`；Android 维持现在的 `cache/`。日志超过 256 KiB 时截断，只保留最近一次启动。同步改 `test/diag_stage_test.dart`。
2. `scripts/package_windows.ps1`：
   - `git status --porcelain` 非空时默认失败，提示用 `-AllowDirty`；
   - 加 `-AllowDirty` 开关，放行时在包内清单写一行 `WARNING: built from a dirty worktree` 与 `git status --porcelain` 的条目数；
   - 在脚本头部注释写明：从 Git Bash 调用时，布尔参数要写成 `'-SkipFlutter:$false'`（带引号），V3 遇到过展开成空串的问题。
3. `docs/STORAGE_AND_PLATFORMS.zh-CN.md` 加一节“Windows 卸载时要手动删除的目录”，按 V4 的记录与本项的新位置列出（只写目录类别，不写真实路径）。

验收：干净工作区打包成功；人为制造一个未提交改动后默认失败、加 `-AllowDirty` 成功且清单含警告行；V4 的 Windows 步骤重做一次，包外新写的只有身份目录下的 `logs/`，§8 原样记录。

### 4.6 W6：性能实测（V-N5）

- Android：`flutter build apk --profile`，`adb install` 后冷启动三次（每次先 `adb shell am force-stop`），用 `adb shell am start -W -n dev.ak.ak_sending_app/.MainActivity` 记 `TotalTime`，再从 logcat 找 Flutter 首帧时间；§8 原样贴三次输出。
- Windows：release 包冷启动三次，记从启动到主窗口出现的时间（沿用 V4 的测法）。
- 任一平台首帧超过 3 秒：用 Flutter DevTools 的 timeline 或在 `main.dart` 的启动阶段打点，§8 写出最慢的一段；本批只定位，不要求修。

### 4.7 W7：三处交互小改

不改协议，不改两个一级入口。

1. **加入时不再选方向。** 契约写明“加入方得对侧角色”。加入面板去掉方向选择，`joinCode` 不传 `role`；配对成功的提示改为按实际角色说明：“对方将把文件发给你”或“现在可以选择文件发给对方”。若引擎在不传 `role` 时有任何不同行为，先停下在 §8 记录，交复核方。
2. **连接成功后直接进入下一步。** 首页配对流程到达成功状态后：本机是发送方，自动打开该连接的详情页并弹出文件选择（用户取消选择则停在详情页）；本机是接收方，成功卡片显示“等待对方发送文件…”与保存位置，不再需要点“完成”。
3. **传输期间保持屏幕常亮（Android）与前台提示。**
   - 在 `MainActivity` 里加一个窄的 `MethodChannel`（例如 `dev.ak/screen`），两个方法：开、关 `FLAG_KEEP_SCREEN_ON`；不加第三方插件；
   - Dart 侧在任一传输进入进行中时打开，全部结束（完成、失败、取消）时关闭；Windows 与 Linux 上为空操作；
   - 进行中的传输卡片在 Android 上加一行提示：“传输期间请保持本应用在前台”；
   - 所有者若对 U-N2 选了 B，本条只做提示，不做屏幕常亮。

验收：每处改动都有 widget 测试（平台通道用假的 handler）；W9 的 V2-a 一轮里，传输期间 `adb shell dumpsys power | grep -i "SCREEN_BRIGHT\|FULL_WAKE\|keep"` 能看到屏幕常亮的持有，传完后消失，§8 原样贴两次输出。

### 4.8 W8：归档已关闭批次的文档（可选）

E4（2026-11-12）已决定“已关闭的报告归档”。只在 W0–W7 都完成后做，单独一个提交：

- 把批次 F 到 T 的交接与复核、K2–K24 各轮复核移到 `docs/archive/`（用 `git mv`）；批次 U、V 与本批留在原处；
- 修好所有指向它们的链接（`check_repository.py` 会报出全部断链）；
- STATUS 的“交接入口”只保留当前批次、上一批、审核文档和 `docs/archive/` 目录链接；
- 不改归档文件的内容。

做不完就整项不做，在 §8 写明，不要留半归档的状态。

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
cd flutter_app && flutter analyze && flutter test && cd ..
cd flutter_app/packages/ak_ffi_dart && dart test && cd ../../..
```

补充说明：

- 本机 Rust 用 rust-toolchain.toml 钉的 1.99.1；
- 本批改了 Dart 与平台工程，Flutter 与 `ak_ffi_dart` 的测试必须在本机跑，不能只依赖 CI；
- 改了 `ak-ffi` 就重跑 `bash scripts/build_android_ffi.sh` 并在 §8 记 x86_64 一行，T1 回归在新库上跑；
- 缺工具时写“未执行”，不能写成通过；
- 推送前在最终文档上跑一遍 `python scripts/check_repository.py`。

## 6. 文档与分支规则（摘自 AGENTS.md）

- 每项状态只写在状态表里，提交列写真实提交号，不写“本提交”；
- 状态表的“证据摘要”列不写运行编号、计数、倍数和带小数或区间的耗时；这些数字只写在本文 §8；
- 新增的行放在本批前一批各行之后：先 W0–W9，复核方的 W-N 行由复核方加；
- 一项被后续批次确认或推翻时，原地改写它的那一行。V-N2 至 V-N6 由对应的 W 项原地补充结论，保留原有描述；
- STATUS 只写阶段结论、下一步和链接，整份改写时保留全部入口链接（W8 做了归档的除外，见 §4.8）；
- 状态表表尾的说明行由复核方写，执行方不要加；
- 执行方只推 `dev/batch-w-<日期>`；任何人都不强推，发现错误就追加提交；
- 新增的 CI 门槛失败时先查原因，不要为了通过而削弱它；
- 所有失败都记入 §8，不能只记最后一次；
- 验收要求原样贴出的输出每次都要贴；
- 结论要与同一行的数字一致；
- 没有做的项，在 §8 写明没有做和原因。

## 7. 红线

- 不引入 TURN/relay，服务器不经手文件数据（ADR-001）；
- 不部署、不登录、不修改生产服务器，也不改它的防火墙；走查一律显式指向本机的注册服务器与 STUN；
- 不生成签名密钥，不改 `applicationId`；
- 不新增第三方依赖；不改线上协议；
- 配对密钥、令牌、用户文件、真实文件名或路径、完整网络诊断都不入库；任务码与配对码不入库，指纹与 job id 只写前 8 位；截图不入库；
- 模拟或同机测试不写成真机验证。

## 8. 证据表（执行方填写）

执行方在本机 Windows 与 Android 模拟器上工作。按 W0–W9 的顺序逐行填写；每行写明环境（本机 Windows、模拟器、debug 或 profile 构建）、提交或运行编号、原样输出与结论。

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
