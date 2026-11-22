# 交接文档：批次 X（2026-11-22）

> 起点：main 快进到批次 W 复核的最终文档提交，本文随该提交入库。
>
> - 批次 W 的结论见 [W 复核](REVIEW_2026-11-22-batchW.zh-CN.md)；
> - 全项目审核与界面设计方案见 [全项目审核](AUDIT_2026-11-22.zh-CN.md)（AUD-05 已更正）；
> - 复核方的命令与运行编号见 [W 交接 §9](HANDOFF-BATCH-W-2026-11-22.zh-CN.md#9-复核方证据2026-11-22)；
> - 执行方从本文 §8 证据表开始记录本批的运行编号与计数；
> - 每项状态只写在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

## 0. 一句话现状

批次 W 有条件通过：僵尸连接行（V-N2）在引擎与应用两层修好，三种失败情形的走查一致；错误文案映射、应用名、打包卫生、性能定位、屏幕常亮都合格。一项被推翻：W7.1 的“加入时不选方向”让跨设备加入在对端建“我要接收”码时必然失败（W-N1，P1），根因是复核方对契约的误读；复核方已把加入面板的方向选择还原，只经 CI 验证。V-N3（黑屏）因证据失效改回待验证。

**批次 X 的三块：**

- 先还清 W 复核登记的欠账：X1 补验 W-N1 并加护栏（P1），X2 修 W-N2 至 W-N5、W-N8，X3 补验 V-N3；
- 再做界面改版里不依赖新依赖、也不等所有者决定的两件：X4 新首页与两条主路径（方向只声明一次，这才是 AUD-05 的正确解法），X5 接收确认显示设备名与内容摘要；
- 收尾：X6 CI 增加 Windows 构建与 release APK（可选），X7 归档（可选），X8 全部门槛与回归走查。

## 1. 复核方在批次 W 分支上的改动

| 项 | 内容 | 主要位置 |
|---|---|---|
| W-N1 | 还原加入面板的方向选择与被改的五处测试；契约措辞澄清；审核 AUD-05 勘误 | `flutter_app/lib/ui/pages/home_page.dart`、`flutter_app/test/`、`flutter_app/integration_test/k1_android_saf_test.dart`、`docs/contracts/ffi-v2.md`、`docs/AUDIT_2026-11-22.zh-CN.md` |
| W-N6 | 恢复 W 交接 §8 的三处路径；`check_repository.py` 新增“路径分隔符被吃掉”检查与两条测试 | `docs/HANDOFF-BATCH-W-2026-11-22.zh-CN.md`、`scripts/check_repository.py`、`scripts/tests/test_repository_checks.py` |
| 文档 | W 复核；W 交接 §9；状态表（W1、W2、W3、W7 行的提交与结论，main 合并一行，新增 W-N1 至 W-N8）；STATUS；本文 | `docs/` |

产品代码的改动只有 W-N1 一处（对 W7.1 第 1 项的还原）；没有改 Rust。

## 2. 接手前必读

### 2.1 报完成之前与分支

沿用 W 交接 §2.1：

- 最后一次推送之后，运行 `git ls-remote --heads origin`，确认远端有 `dev/batch-x-<日期>` 且指向你的最后一个提交；
- 等这次 CI 全部跑完再报完成；
- 失败了，先在 §8 记下运行编号、失败的任务与测试、完整报错，再追加一个提交推送，重跑通过也照样记失败那次；
- §8 不留占位；回复里写明最后一次推送的提交号、CI 运行编号与结论；
- 推送后的每一个提交都要有对应的 CI 记录，不要只记中间的那一个（W 的“最终 CI”一行就记在了中间提交上）。

### 2.2 往文档里写内容（V-N1、W-N6 的教训）

两批里反斜杠转义吞掉文档内容共出现两次。要求加严：

- 路径在文档里**优先写正斜杠**（`logs/ak-stage.log`），必须写反斜杠的 Windows 路径用编辑器，或带引号的 heredoc（`cat <<'EOF'`）；不要用 `echo -e`、不带引号的 heredoc 或 `printf` 的格式串；
- `check_repository.py` 现在拦两类损坏：控制字符（V-N1），以及路径占位符（`%TEMP%`、`<home>`、`<repo>`、`<tmp>`）后直接接字母的“分隔符被吃掉”（W-N6）；它报错时先看是不是转义问题；
- 写完 §8 之后用 `grep -n` 看一眼含路径的几行，不要只依赖检查。

### 2.3 测试替身与真实引擎（W-N1、W-N7 的教训）

W7.1 的缺陷能通过全部自动化测试，是因为同进程的 Rust 测试和 Dart 假服务都镜像了“加入方得对侧角色”，而真实跨设备路径不是这样。要求：

- 界面行为一旦依赖引擎语义（角色、方向、终止原因、缺省值），先找到实现它的那行 Rust 代码，在 §8 写出文件与函数名；找不到就停下来记录；
- 假服务的行为必须与真实引擎一致；不一致的地方在假服务里写明，并在 Rust 一侧有钉住真实行为的测试；
- 引用契约文字推出行为时，同时核对代码；两者不符以代码为准，并改契约措辞。

### 2.4 本批会改产品代码

- 每个修复与功能都要有能在 CI 跑的自动化测试，只有走查结果的不算完成；
- 改 FFI 的 JSON 字段或事件时先改 `docs/contracts/ffi-v2.md`，保持纯加法，并跑 `python scripts/check_contracts.py`；
- 不改线上协议（`ak-wire`、`protocol-v2.md`）；需要改协议时停下，在 §8 写明原因，交复核方；
- 不新增第三方依赖（ADR-002）；Flutter SDK 自带的包也先不引入；
- 改动的现有测试断言在 §8 列出：哪个测试、旧断言、新断言、为什么；
- X4 会改动大量带冻结键的测试与走查 runner，键的变更清单也要在 §8 列出。

### 2.5 Android 走查的共同做法

沿用 W 交接 §2.4，另加两条（W2 的教训）：

- 每次截屏的同时记录前台窗口归属：`adb shell dumpsys window | grep -i mCurrentFocus`，并核对它指向本应用；
- 画面“正常”不能只靠亮度均值，要同时有可操作的证据（例如 `uiautomator dump` 里能找到本应用的某个控件，或 runner 的 `PROGRESS` 行仍在推进）。

## 3. 全项目未完成事项

### 3.1 所有者事项（agent 不得代办）

见 [审核 §6](AUDIT_2026-11-22.zh-CN.md#6-需要所有者决定的事项)，当前全部未决：

- O-1（U-N2）Android 后台策略：复核方建议 A；
- O-2（P2-02）移动数据；
- O-3 应用显示名：“AK互传”暂用，所有者可改；
- O-4 应用图标与 `applicationId`；
- O-5 v1 平台范围；
- O-6 新依赖审批（二维码显示与扫码）：本批不做依赖这项的内容；
- O-7 流程瘦身；
- E5 签名、E6 部署、E7 许可证、C4 物理双机，见 [状态表 §8](REVIEW_2026-11-11.zh-CN.md#8-需所有者确认的事项)。

外部 agent 对 O-1、O-3、O-6 的“预审”只是参考，不是所有者的决定。

### 3.2 工程项

- 依赖所有者批准的界面项（二维码显示与扫码、Windows 拖放、Android 分享入口与通知、`gen-l10n`）排在批次 X 之后，见 [审核 §5](AUDIT_2026-11-22.zh-CN.md#5-分阶段路线)；
- 宽屏两栏布局、最近传输列表、附近设备、PAKE 短码（批次 Y，需要先写 ADR-003）不在本批；
- 其余沿用 W 交接 §3.2。

## 4. 批次 X 工作单

| 项 | 步骤 | 验收 |
|---|---|---|
| X0 | 1) `git fetch --prune`；2) 确认 `git merge-base --is-ancestor origin/dev/batch-w-20261122 origin/main` 成立，然后删除这个远端分支；3) 切到 main 并 `git pull --ff-only`，删除本地旧分支，执行 `git worktree prune`；4) 从 main 切出 `dev/batch-x-<日期>`；5) 在 §8 回填 main 上本文所在提交的 CI 运行编号与结论 | `git branch -a` 只剩 main 和本批分支；`git worktree list` 只剩主目录；两条命令的输出原样贴进 §8；§8 有回填行 |
| X1 | W-N1 补验与护栏，按 §4.1 | §4.1 的四条全部满足 |
| X2 | W-N2、W-N3 与 W-N8、W-N4、W-N5，按 §4.2 | 每项都有自动化测试；被改断言在 §8 列出 |
| X3 | V-N3 补验，按 §4.3 | 五轮都记录前台窗口归属与可操作证据；结论按 §4.3 |
| X4 | 新首页与两条主路径，按 §4.4 | §4.4 的验收全部满足，或整项不做并写明 |
| X5 | 接收确认显示设备名与内容摘要，按 §4.5 | §4.5 的验收全部满足，或整项不做并写明 |
| X6 | 可选：CI 增加 Windows 构建与 release APK，按 §4.6 | CI 通过且没有削弱现有任务 |
| X7 | 可选：归档已关闭批次的文档，按 §4.7 | `check_repository.py` 通过；STATUS 的链接明显变短 |
| X8 | §5 的全部门槛；X4 之后的 V2-a 一轮；X1 的两个新方向各一轮；T1 回归一轮 | 全部通过；走查结论写在 §8 |

做的顺序：X0、X1、X2、X3 先做；X4 是本批主体；X5 以后按顺序做到哪算哪，**X4 至 X7 都是整项交付或整项不做**，不留半改版。

### 4.1 X1：W-N1 补验与护栏（P1 收尾）

复核方已把加入面板的方向选择还原（只经 CI 验证）。本项补三件事：

1. **双引擎 Rust 测试**，放在 `crates/ak-ffi/src/task/task_tests.rs`，参照 `all_failed_probe_settles_unreachable_not_connected` 里 `engine_a`、`engine_b` 的写法。三种情形：
   - 创建方 `role:"receiver"`，加入方 `ak_task_join` **不传 `role`**：预期加入失败（创建方方向校验拒绝同向配对）。**这一条先写，预期失败；记录加入方看到的错误原文。如果它竟然成功，说明引擎有复核方没有看到的推断路径，停下来在 §8 写明，交复核方**；
   - 创建方 `receiver`，加入方 `role:"sender"`：成功；
   - 创建方 `sender`，加入方不传 `role`：成功，且加入方的角色是 receiver。
   
   这三条钉住“跨设备缺省为 receiver、同向被拒”。
2. **假服务对齐**：`flutter_app/lib/services/simple_transfer_fake.dart` 的 `joinCode` 在 `role == null` 时缺省为接收（与真实引擎一致），依赖旧缺省（创建方的对侧）的测试改成显式传 `role`；被改断言在 §8 列出。
3. **两个新方向的跨进程走查**（真机或模拟器，传一个小文件，两端 SHA-256 相等）：
   - App 作为创建方，选“我要接收”，宿主 CLI `join-send`；
   - 宿主 CLI 作为创建方并接收，App 作为加入方并在加入面板选“发送文件”。
   
   这两个正是 W7.1 弄坏的组合；原有的“App 创建发送 + CLI 接收”“CLI 创建发送 + App 加入接收”两个方向已在 V、W 的走查里覆盖，不必重做。

### 4.2 X2：W 复核遗留修复

| 项 | 内容 | 验收 |
|---|---|---|
| W-N2（P2） | 停滞看门狗不再对校验阶段（进度阶段 3）生效，或预算按字节数放大——二选一，在 §8 写明理由；校验阶段引擎只在每个文件校验完成时发进度 | Dart 单元测试：校验阶段超过预算**不**被标 `conn_lost`，传输阶段仍被标；W1 现有的看门狗测试全部保持通过 |
| W-N3 与 W-N8（P3） | 发送场景的“传输中断了”文案：原因保持中性（“连接断开了，或发送过程中出了问题”），建议改为“请重新配对后再发送，会从断点续传”；接收与配对场景保持现状；原文仍在“详情” | `transfer_errors_test.dart` 与 widget 断言更新；连接详情页的状态行同步 |
| W-N4（P3） | `-2`（`AK_ERR_INVALID`）只在加入场景映射为“这个码用不了”；发送与启动场景走词表与兜底 | 表驱动测试覆盖三个场景 |
| W-N5（P3） | 把 `post_connect_verdict` 与它的两个判定（TASK-CLOSE 的 `Cancelled` 码加消息、DECLINED 的码加消息）下沉到 `ak-engine`，FFI 与 CLI 共用；CLI 的 `task_cli.rs` 两处单属性匹配改用它；测试随之搬到 `ak-engine`，FFI 侧保留集成测试 | `cargo test`、`clippy -D warnings`、`cargo doc -D warnings` 通过；CLI 对“同码异文、同文异码”的反例行为与 FFI 一致 |

### 4.3 X3：V-N3 补验（黑屏）

沿用 W 交接 §8 里 W2 一行的补验计划，并加严记录：

- 构建与轮数：profile 三轮、debug 两轮（debug 允许手工进入传输，以避开弹层点击故障）；
- 每轮：传输中按 Home，120 秒后恢复同一 Activity；恢复后 5 秒与 30 秒各记一次：前台窗口归属（`mCurrentFocus`）、截屏亮度均值、一条可操作证据（见 §2.5）；
- 任一轮在“前台是本应用”的前提下窗口全黑：按 W 交接 §4.2 第 3 条做渲染后端对照（`--no-enable-impeller` 一轮）与修复，修复后 profile 三轮都正常才算完成，并补一条断言回前台请求了新帧的 widget 测试；不要靠改渲染配置“关闭”问题；
- 五轮都正常：以“未复现”关闭 V-N3，在状态表原地改写该行；
- profile 构建在模拟器上 UI 自动化不稳定的问题，先试用手工进入传输加 adb 触发的办法；做不到的轮次如实写“未执行”及原因，不要用别的轮次顶替。

### 4.4 X4：新首页与两条主路径

这是本批主体。目标是实现 [审核 §4.3](AUDIT_2026-11-22.zh-CN.md#43-关键流程) 的主路径，并借此让方向**只声明一次**（W-N1 的正确解法，见 [W 复核 §4](REVIEW_2026-11-22-batchW.zh-CN.md#4-免选方向的正确做法)）。不改协议、不新增依赖、不做二维码。

| 入口 | 主路径 | 次要入口 |
|---|---|---|
| 发送文件 | 先选文件（可多选，含文件夹），再生成码（创建方为发送方）；码卡片显示大号分组码、复制、已选摘要与“更改”；对方加入且连通后**自动开始发送**，不再需要点任何按钮 | “对方已经给了你一个码？输入对方的码”（加入方为发送方，方向已知；成功后沿用 W7.2 的自动弹文件选择） |
| 接收文件 | 输入对方的码（加入方为接收方，方向已知），成功后沿用 W7.2 的等待卡 | “想让对方输入你的码？生成接收码”（创建方为接收方） |

要求：

1. 去掉创建前的角色面板（`_RoleSheet`）与加入面板的方向选择；`PairingFlowController.startJoin` 的 `role` 改为必填，界面不再有“没有方向”的路径。
2. 方向只来自第一次点击：四个入口组合各有一条 widget 测试，断言传给服务的 `createCode` 与 `joinCode` 的角色，并且 `joinCode` 的角色永不为 null。
3. 自动发送：发送路径在连接建立后自动调用发送；连通测试未通过（“仍可尝试发送”）时同样继续尝试，不因探测结果阻断；发送失败按 W3 的错误卡显示。
4. 文案按 [审核 §4.5](AUDIT_2026-11-22.zh-CN.md#45-文案规范)：对方、连接、无法直接连接；标题说结果、正文说原因与办法、按钮用动词。只改新首页与这两条路径里的字符串，其余界面留待后续。
5. 冻结键：新增 `home_send_button`、`home_receive_button`，次要入口使用新键；旧的 `home_create_code_button`、`home_join_code_button`、`role_send_button`、`role_receive_button`、`join_role_selector` 等凡是被删除或改义的，在 §8 列出，并同步更新所有引用它们的测试与走查 runner（`u2_android_lifecycle_test.dart`、`k1_android_saf_test.dart`、`app_e2e_test.dart`、`android_real_test.dart` 等）；
6. 假服务沿用 X1 的缺省，不要为让测试通过而放宽。
7. 不在范围：宽屏两栏、最近传输列表、二维码、扫码、拖放、分享入口、通知。

验收：`flutter analyze`、`flutter test` 通过；V2-a 在新首页上走一轮（后台 30 秒，传输全程同任务恢复并传完，屏幕常亮的持有与释放成对）；X1 的两个新方向各一轮在新首页上也通过。做不完就整项不做：保留现状（W-N1 的选择面板方案已可用），在 §8 写明原因。

### 4.5 X5：接收确认显示设备名与内容摘要

现状：`incoming_request` 事件只有 `root`、`files`、`bytes`，横幅写“对方请求发送 N 个文件”，对方名称是“对端设备”。目标：确认前让用户知道“谁要发什么”。

1. 先读代码：配对时双方的 endpoint blob 里带一个设备名（`PeerEndpoint.name`）。确认这个名字在 FFI 一侧能拿到哪里；在 §8 写出文件与函数名；
2. 如需新增字段（例如 `connected` 事件与 `ak_task_list` 行的 `peer_name`，或 `incoming_request` 的 `peer`）：先改 `docs/contracts/ffi-v2.md`，纯加法，跑 `check_contracts.py`，Dart 解析器容忍缺省；
3. 横幅与确认面板显示：发送方设备名、`root`（顶层名称）、文件数与总大小、保存位置；**设备名是对方自报，不是身份证明**：在名称旁标注指纹前 8 位的短码，不把名称写成“已验证”；
4. 不显示单个文件名（“最近传输不含文件名”的持久化隐私约定不变）；
5. 测试：假服务可注入设备名；widget 测试断言展示与缺省（拿不到名字时仍显示“对方的设备”）。

做不完就整项不做，在 §8 写明。

### 4.6 X6：CI 增加 Windows 构建与 release APK（可选）

[审核 AUD-21](AUDIT_2026-11-22.zh-CN.md#5-分阶段路线)：界面相关的发布构建不在 CI 里，回归只能靠本机。只做两件，且只验证“能构建”：

- Windows：在 CI 里 `flutter build windows --release`；
- Android：`flutter build apk --release`，使用模板的默认签名配置，不生成签名密钥，不上传制品。

要求：不削弱现有任务；新任务失败时先查原因，不要为了通过而降低门槛；CI 时间明显增加时在 §8 记录数字。

### 4.7 X7：归档已关闭批次的文档（可选，沿用 W8）

E4（2026-11-12）已决定“已关闭的报告归档”。只在 X1 至 X5 完成后做，单独一个提交：

- 把批次 F 到 T 的交接与复核、K2–K24 各轮复核移到 `docs/archive/`（用 `git mv`）；批次 U、V、W 与本批留在原处；
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
- 本批改了 Rust（X1、X2）、Dart 与界面，Flutter 与 `ak_ffi_dart` 的测试必须在本机跑，不能只依赖 CI；
- 改了 `ak-ffi` 或 `ak-engine` 就重跑 `bash scripts/build_android_ffi.sh` 并在 §8 记 x86_64 一行，T1 回归在新库上跑；
- 缺工具时写“未执行”，不能写成通过；
- 推送前在最终文档上跑一遍 `python scripts/check_repository.py`。

## 6. 文档与分支规则（摘自 AGENTS.md）

- 每项状态只写在状态表里，提交列写真实提交号，不写“本提交”；
- 状态表的“证据摘要”列不写运行编号、计数、倍数和带小数或区间的耗时；这些数字只写在本文 §8；
- 新增的行放在本批前一批各行之后：先 X0–X8，复核方的 X-N 行由复核方加；
- 一项被后续批次确认或推翻时，原地改写它的那一行。W-N1 至 W-N8 与 V-N3 由对应的 X 项原地补充结论，保留原有描述；
- STATUS 只写阶段结论、下一步和链接，整份改写时保留全部入口链接（X7 做了归档的除外，见 §4.7）；
- 状态表表尾的说明行由复核方写，执行方不要加；
- 执行方只推 `dev/batch-x-<日期>`；任何人都不强推，发现错误就追加提交；
- 新增的 CI 门槛失败时先查原因，不要为了通过而削弱它；
- 所有失败都记入 §8，不能只记最后一次；
- 验收要求原样贴出的输出每次都要贴；结论依赖的关键输出行原样贴进 §8（路径与指纹按红线处理），走查原始文件留在本机即可；
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

执行方在本机 Windows 与 Android 模拟器上工作。按 X0–X8 的顺序逐行填写；每行写明环境（本机 Windows、模拟器、debug 或 profile 构建）、提交或运行编号、原样输出与结论。

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
