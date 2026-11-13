# 交接文档：客户端 UI U1（2026-11-13，Claude 实现，待本地 agent 验收）

> **来源**：所有者指示——批次 H/I 复核没有问题就由 Claude 直接做客户端 UI。复核结论见 [REVIEW_2026-11-13-batchH](REVIEW_2026-11-13-batchH.zh-CN.md)。
> **分支**：`review/claude-batchh-ui-20261113`（基于 `97fac96`），PR #15 → `dev/review-fixes-20261111`。
> **验收方**：本地 agent（批次 J1）。这是 Claude 自己写的代码，Claude 不给自己验收。

## 0. 一页结论

- **做了什么**：
  1. 实时传输进度：发送卡和接收横幅显示已传/总量、百分比、速度、剩余时间、当前文件名（只取末段）；字节还没动的时候，如实说明在等什么。
  2. 一次配对一次传输的正常收尾：传输完成、引擎收回连接行之后，详情页显示"已完成"和返回按钮，不再误报"此连接已断开（对端离开或网络变化）"，发送卡保留结果（修复 CR-P2-16）。
  3. 连接卡上的细进度条和一行说明（"发送中 27%""等待对方接受…"）。
  4. "最近传输"：本次运行期间的内存记录，可清空。
  5. 浅色/深色跟随系统；"成功"的绿色改为随主题取色（原来硬编码的深绿在深色背景上看不清）。
- **契约只增不改**：
  - ffi-v2 §8.4 的 U1 修订：任务面进度行携带与引擎级进度行相同的载荷字段；
  - UI 契约新增 `TransferPhase`、`TransferProgress`、`SimpleTransferService.transferProgress`、`IncomingRequest.connId`。
  - 旧 DLL 只发 `job`/`event` 两个字段，新界面这时显示不确定进度条，不猜百分比。
- **验证边界**：
  - Rust 侧本地门槛全过；
  - Flutter `analyze`/`test` 只在 CI 跑过（本环境装不了 Flutter SDK），结果见 §4；
  - **没有任何真机或模拟器走查**，这是 J1 的任务。

## 1. 提交清单

| 提交 | 内容 |
|---|---|
| `b007b16` | U1 主体：Rust 任务面进度行带载荷字段、Dart 进度折叠、适配层映射、UI 契约增补、界面、fake 测试钮、新测试两份、契约文档 |
| `d0eab21` | `flutter analyze` 的 `use_null_aware_elements` 提示（CI run 43432260578 发现） |
| `ab3d6e5` | 新 widget 测试等页面返回动画结束再断言（CI run 43432599231：192/193，失败的正是这条） |

## 2. 改了什么

### 2.1 Rust（`crates/ak-ffi`）

- `src/task.rs`：发送与常驻接收两处进度回调改调 `core::push_event(&queue, &conn_hex, None, &event)`，任务面行因此和引擎级行一样带载荷字段：
  - `offered`/`accepted` 带 `files`/`bytes`；
  - `file_started` 带 `file`/`path`/`bytes`；
  - `chunk_sent`/`chunk_received` 带 `file`/`chunk`；
  - `bytes_acked` 带 `file`/`bytes`；
  - `file_committed` 带 `file`/`path`；
  - `final_ack` 带 `verified`。
  `job` 仍是连接 id，不带 `conn` 序号。测试 `receive_progress_rows_ride_under_the_connection_id` 补了三条断言。
- `src/core.rs`：删除不再使用的 `progress_name`。`src/lib.rs` 的"progress event lines"一节注明任务面行的形状。
- `docs/contracts/ffi-v2.md` §8.4：U1 修订段（字段、`path` 只在进程内存队列里，旧引擎降级规则）。`check_contracts.py` 5/5。

### 2.2 Dart 服务层

- `lib/services/simple_transfer_service.dart`（引擎服务）：
  - 新增 `SimpleTransferProgress` 和私有的 `_TransferTracker`。每次 `drainProgress` 结束时，每个传输至多发出一个快照（`finally` 里发，提前返回也不漏）。
  - 阶段只升不降：`connecting → awaiting_consent（仅发送方）→ transferring → verifying → completed`。"所有字节已动"时自动进入 `verifying`：发送方此时在等接收方校验。
  - 字节数取每个文件"已确认字节"与"块位置 × 1 MiB"中较大者，不超过文件大小；已完成的文件移出在途表，所以快照的代价只跟在途文件数有关。
  - 只为**本服务的活连接**建跟踪器，未知 id 的行和终态之后的迟到行一律忽略。
  - 任务 `done` 事件先到时直接补发 `completed`；连接终态时跟踪器随行删除。
  - 字段缺失或类型不对时一律当"未知"，**不计入** C12 的畸形行计数。畸形行的失败原因只记字节数，不再引用原文（原文可能含相对路径）。
  - 可注入时钟（`clock`）。
- `lib/services/ffi_simple_transfer_service.dart`：
  - `transferProgress` 映射到 UI 契约，未知阶段名按 `transferring` 处理；
  - `IncomingRequest.connId` 透传；
  - 发送摘要追加直连路径标签（来自 `task_send` 结果的 `peer`，即本次实际使用的地址）。
- `lib/services/direct_path_label.dart`（新）：按地址类别给标签，规则见 §3。放在服务层是因为要用 `dart:io` 解析地址，UI 层保持不依赖 `dart:io`。
- `lib/services/simple_transfer_fake.dart`：新增 `emitProgress`、`endConnection`（模拟引擎收回已完成的行），`emitIncomingRequest(connId:)`。

### 2.3 UI 契约与界面

- `lib/ui/service/simple_transfer_service.dart`：上述契约增补（只增不改，沿用 `engineHealth` 的先例）。
- `lib/ui/widgets/transfer_progress_view.dart`（新）：进度条、状态行、速度与剩余时间、当前文件。旧 key 不变：`conn_send_progress`、`incoming_progress_<job>` 仍是进度条本身。
- `lib/ui/pages/connection_detail_page.dart`：
  - 保留最后一次见到的行（`_lastKnown`）；
  - 完成后行消失，判为"已完成"：key `conn_state_ended`、`conn_back_home_button`；
  - 结果显示用时与平均速度（`conn_send_result_stats`）；
  - 成功和失败都写入"最近传输"。
- `lib/ui/widgets/incoming_requests_host.dart`：横幅接收中显示进度；完成后显示用时与平均速度（`incoming_summary_<job>`）；写入"最近传输"，对端标签取自连接列表。
- `lib/ui/widgets/task_connection_views.dart`：连接卡的细进度条（`conn_card_progress_<id>`）和说明（`conn_card_caption_<id>`）。
- `lib/ui/pages/home_page.dart`："最近传输"一节（`history_section`、`history_clear_button`、`history_item_<i>`，最多显示 10 条）。
- `lib/ui/service/transfer_history.dart`（新）：`TransferHistory`（内存、上限 50 条）与 `TransferHistoryScope`（放在 MaterialApp 之上，推入的详情页与首页共用一份）。
- `lib/ui/service/transfer_format.dart`（新）：速度、用时、剩余时间的纯函数。
- `lib/ui/theme.dart`（新）：浅色/深色主题和随主题的成功色。
- `lib/ui/app.dart`：`AkSendingApp` 改为有状态，持有历史记录；`themeMode: ThemeMode.system`。

### 2.4 测试

- `test/transfer_progress_test.dart`（新，服务层）覆盖：
  - 发送方全阶段折叠（每次 drain 一个快照、块位置细化、速度、完成后不再重开）；
  - 旧 DLL 行；类型异常的字段；
  - 接收方不进入等待接受阶段；
  - 未知 id 被忽略；
  - 任务 `done` 先到；中止的传输不发 `completed`；
  - 适配层阶段映射、发送摘要带路径标签、`IncomingRequest.connId`；
  - 格式化函数、路径标签、历史记录、主题色。
- `test/transfer_ui_test.dart`（新，界面）覆盖：
  - 发送卡的等待接受、进度、速度、剩余时间、当前文件；
  - 其他连接的进度不串页；
  - 完成后行被收回时的"已完成"状态，以及最近传输；
  - 连接卡说明；
  - 接收横幅进度与完成摘要；
  - 失败记录与清空；
  - 深色模式。

## 3. 行为与取舍（走查时据此判断"是不是 bug"）

- **进度精度**：
  - 引擎每 16 个确认报一次 `bytes_acked`。块位置把进度细化到 1 MiB（任务面用引擎默认块大小，不覆写）。
  - 发送方的块位置表示"已发出"，不是"已确认"，所以发送方的进度可能略微领先。
  - 断点续传的文件第一条块行会一次跳到续传位置；整个文件都已续完而不再发送的，要到完成时才计入。
- **速度**：从第一条传输中的行算起的平均速度，不是瞬时速度；不足 1 秒不显示。剩余时间按平均速度估算并向上取整。
- **阶段文案**：

  | 阶段 | 发送方 | 接收方 |
  |---|---|---|
  | 无快照（旧 DLL 或刚开始） | 正在发送… | 正在接收… |
  | connecting | 正在建立连接… | 正在准备接收… |
  | awaiting_consent | 已发出请求，等待对方点「接受」… | — |
  | transferring | 已发送 X / Y（N%） | 已接收 X / Y（N%） |
  | verifying | 数据已全部发出，等待对方校验… | 正在校验文件完整性… |

- **文件计数**：
  - 引擎的 `files` 是清单条目数，目录也算在内。这是既有口径，与 `task_send` 摘要"已发送 N 个文件"一致，本轮没有改。
  - "（i/N）"的 i 也是条目序号。
- **直连路径标签**，只说地址类别，不夸大：

  | 地址 | 标签 |
  |---|---|
  | 私网/链路本地 IPv4，链路本地/ULA IPv6 | 局域网直连 |
  | 其他 IPv4（含运营商级 NAT 段） | 互联网直连 |
  | 全局 IPv6 | IPv6 直连（同一局域网还是跨互联网，单看地址分不出来） |
  | 回环 | 本机直连 |
  | 解析不了 | 不显示 |

  没有中继，所以不存在"中继"标签。
- **"已完成"的判定**：
  - 连接行消失，并且本页的发送已成功，或本连接的进度到过 `completed`；
  - 行消失而传输没有完成的，仍按"此连接已断开"显示。
- **最近传输**：
  - 只在内存里，应用退出即丢失；
  - 只有方向、条目数、字节数、时间、用时、路径标签、对端标签、失败原因；
  - **不含文件名或路径**；
  - 接收方的用时从点"接受"算起。
- **隐私**：进度行里的 `path` 只用来取末段文件名显示在本机界面上，不进诊断导出、日志或历史记录。畸形行的失败原因也不再引用原文。

## 4. 验证证据与边界

- **Rust**（本环境，stable）[已执行]：
  - `cargo fmt --all -- --check`、`cargo clippy --locked -p ak-ffi --all-targets -- -D warnings`、`cargo test --locked -p ak-ffi`（全部通过）；
  - `RUSTDOCFLAGS="-D warnings" cargo doc --locked -p ak-ffi --no-deps`；
  - `check_contracts.py` 5/5，`check_repository.py` 通过，`scripts/tests` 43 项通过。
- **Flutter**（只在 CI）：
  - run 43432260578：analyze 报 1 条提示（已修）；
  - run 43432599231：analyze 通过，测试 192/193（1 条新测试的时序问题，已修）；
  - 最终结果：run [43433201562](https://github.com/wmqfl861/AK-sending/actions/runs/43433201562)（`ab3d6e5`）全部任务成功：flutter analyze 无问题、app 测试 193/193、`ak_ffi_dart` 170/170；Rust ubuntu/windows、Android（双 ABI、符号校验、APK）、NAT 矩阵七案、legacy-signal、audit、deny、checks、portability×3 全部成功（release-deep-tests 为手动任务，按设计跳过）
- **没有做的**：Windows 真机 GUI 走查、Android 模拟器走查、新 GUI 配旧 DLL 的降级实测、真实网络下的进度精度观察。全部列在 §5。

## 5. 本地 agent 走查清单（J1）

**准备**：

- 从合入后的分支重新构建 `ak_ffi`：Windows release DLL，以及 Android 两个 ABI（`scripts/build_android_ffi.sh`）。旧 DLL 不带载荷字段。
- 本机起一个本地注册中心（`ak-cli registry-server`）和 STUN（`ak-cli stun-server`）。GUI 用 `AK_REGISTRY_ADDR`、`AK_REGISTRY_ID`、`AK_STUN_ADDR` 指向本地服务。**不连接、不改动生产服务器。**
- 对端可以用同机的 CLI（`task code-recv`/`task join-send`），或者第二个 GUI 实例（不同的 `AK_ID_DIR`）。
- 测试文件一律用合成文件（例如 `dd`/`fsutil` 生成的 200 MiB 文件，以及一个含若干小文件的目录）。截图和报告里不得出现真实文件名或路径。

**逐条走**（每条记"通过/不通过/现象"）：

1. **发送进度（Windows GUI 发 → CLI 收）**：
   - 按发送后先看到"正在发送…"或"正在建立连接…"（可能一闪而过）；
   - 接收端还没同意时显示"已发出请求，等待对方点「接受」…"，进度条为不确定态；
   - 同意后百分比、速度、剩余时间、当前文件名持续更新；
   - 最后显示"数据已全部发出，等待对方校验…"，然后是"发送完成：…· 本机直连"（或"局域网直连"，取决于实际拨通的地址），以及用时与平均速度。
2. **正常收尾**：
   - 第 1 条完成后，详情页显示"已完成"、"本次传输已完成，配对随之结束……"和"返回连接列表"；
   - 发送结果仍在；**不出现**"此连接已断开（对端离开或网络变化）"；
   - 返回后首页"当前连接"里没有这条，"最近传输"里有一条"发送 N 个文件（大小）"，下面一行是"完成 · 时间 · 用时 · 路径标签 · 对端名"。
3. **接收进度（CLI 发 → Windows GUI 收）**：
   - 横幅点"接受"后显示"正在准备接收…"或直接进入"已接收 X / Y（N%）"；
   - 速度与当前文件更新；然后"正在校验文件完整性…"；
   - 完成卡带"用时 … · 平均 …"；首页连接卡同时显示"接收中 N%"；
   - "最近传输"记一条接收。
4. **目录发送**：选一个含多个小文件的目录发送；"（i/N）"不超过 N；完成后条目数与摘要一致。
5. **失败与拒绝**：
   - 接收端拒绝，发送卡显示失败原因，"最近传输"记一条失败；
   - 发送中途杀掉接收端进程，进度停住，发送卡显示失败，不显示"已完成"。
6. **清空**：点"最近传输"的"清空"，该节消失；重启应用后历史为空（设计如此）。
7. **深色模式**：Windows 切到深色，进度条、成功色、失败色、"已完成"标记都清晰可读。
8. **窄屏**：Android 模拟器竖屏（约 390 dp 宽），长文件名截断而不溢出，进度三行不重叠。
9. **Android 作为发送方**：模拟器 GUI 发 → Windows 收（模拟器在 QEMU NAT 之后，主动发出方向最稳）。路径标签预计为"局域网直连"或"互联网直连"，按实际记录。Android 作为接收方能否打通取决于模拟器 NAT，**如实记录结果**，不作为本条的通过条件。
10. **旧 DLL 降级**：用 `97fac96` 构建的旧 DLL 运行新 GUI 发送一次：
    - 进度条为不确定态，文案为"正在发送…"，不出现百分比；
    - 完成与收尾同第 2 条；
    - 没有"引擎故障"横幅。

**发现问题时**：按"一项一提交"修复，并补 widget 或服务测试；界面文案改动同步本文 §3 的表。

## 6. 已知限制与后续建议

- 速度只有平均值；需要瞬时速度时，可在跟踪器里加滑动窗口（建议 3–5 秒）。
- 断点续传中已完整续完的文件，要到完成时才计入；可以用 `resuming.chunks` 做基数，但要避免与续传位置的块行重复计数。
- 历史记录只在内存里，这是刻意的（隐私），所有者若要持久化需另定口径。
- CR-P3-15 修好之前，"失败后重试"和"闲置超过 10 分钟后再发"在 IPv4 双锥形网络下可能失败，界面目前不提示（见复核文档）。

## 7. 红线遵守

- 没有中继，没有服务器经手文件数据；路径标签只有直连三类加本机。
- 没有访问、部署或改动生产服务器；没有签名密钥；没有真实文件名、路径或 IP 入库（测试只用合成名、回环、RFC 5737/RFC 1918/文档段地址）。
- 契约先行：先改 ffi-v2 §8.4，契约检查 5/5；UI 契约只增不改。
- 没有把 CI 或模拟测试写成真机验证。
- 没有改 main，没有强推，没有合并 PR。
