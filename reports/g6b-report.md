# G6b 报告：ServerInfo revision 的 GUI 显示（G6 Flutter 侧收尾）

- 日期：2026-11-13
- 分支：`dev/review-fixes-20261111`（G6 提交 09d4465 之后）
- 范围：把 G6 已就绪的 `RegistryClient::server_info()`（ak-session，protocol-v2.md §6a）接到
  Flutter 设置页「关于」区域显示注册服务器版本（revision 字符串）。
- 红线：未触碰 Rust 打洞/拨号代码（`task_net`、probe、punch 路径零改动）；纯增量，无已发布签名改动。

## 方案决策（A/B 取舍）

任务书给了两条路径，实测后取 **A（ak-ffi 新增导出）**：

- **B（挂在 G2 诊断导出的平台头里）不可行**：`DiagnosticsExportGateway` 的导出头
  （`flutter_app/lib/services/connection_diagnostics.dart` 的 `diagnosticsPlatformSummary()`）
  是**纯 dart:io 平台事实**（os/os_version/cpus/locale），没有任何服务端信息通道，也不经过
  Rust——B 要成立仍需先有 A 的 FFI 通道，等于 A+B 两层改动。
- **A 的形态**：不是一个同步查询导出，而是**任务面新增一个异步 op**
  `ak_task_server_info(engine, runtime_id, op_id_out)`。原因：契约 §2/ADR-003 冻结
  “网络等待不能发生在同步 FFI 调用里”，而 `server_info()` 是带 response_timeout 的线上问答；
  op 结果经既有 `ak_pairing_op_result` 通道读取（wave-28 冻结的“结果不新增符号”裁决），
  事件面照常 `op_done`。这使 A 的 Rust 侧薄改只有：一个 `TaskOpSpec` 变体 + 一个安全层入口
  + 一个指针适配导出。

## 实现

### Rust（crates/ak-ffi）

- `task.rs`：`TaskOpSpec::ServerInfo` 变体；`run_server_info()`——`ensure_stack` 后经
  `stack.pairing.orchestrator().client().server_info().await` 取答案，渲染
  `{"op":"task_server_info","revision":"<build>"}`；**`"revision":null` 永远不是错误**
  （G6 的降级口径原样透传：旧服务器无能力位从不被发问、查询失败、断连均如实降级）。
  op 失败仅保留一种诚实场景：registry 栈本身未能建立（未启动/已停止/拨号失败）。
  安全层入口 `task_server_info_start()`（无 spec、无幂等键——Rust 侧按连接世代缓存＋单飞行，
  重复调用开销可忽略）。
- `task_ext.rs`：`ak_task_server_info` 导出（输出指针先检、失败零副作用、`guarded` 包裹）；
  ABI 形状钉死测试；testabi 适配器。
- `lib.rs`：符号再导出＋ crate 文档登记（§G6 follow-through 段）。
- 测试：`TestRegistry` 增加 `spawn_with_revision`（真 `serve_v2_with_revision` 路径）；
  新增 2 个测试——真注册中心往返（含重复查询命中缓存）＋指针纪律/未知 runtime `-4`/
  不可达注册中心如实 op 失败（不是 null）。

### 契约（docs/contracts/ffi-v2.md）

- §8 表头计数说明＋```abi 块新增 `ak_task_server_info` 签名（§8.6 门铃同一“新增符号扩展”
  路径，§1 冻结 18 签名零改动）。
- **新增 §8.7 散文登记**：形态、结果 JSON、null 降级口径、无幂等键理由、Dart 侧旧 DLL
  容忍策略。
- `scripts/check_contracts.py`：**5/5 PASS**（检查 2 的 18 签名口径只数 §1 的 ```c 围栏，
  `ak_task_` 前缀不在其过滤器内——与 §8.6 增补同一先例，无漂移）。

### Dart 包（flutter_app/packages/ak_ffi_dart）

- `AkTaskCalls` 新增 `serverInfo()` 无指针 seam 方法；`AkTaskLeaseGate`（C07）转发；
  `AkTaskFfiBindings` 以**容忍式查找**解析导出（`akTaskServerInfoExportName` 不进冻结门列表
  ——旧 DLL 缺失不触发 §2 硬失败，与门铃同一先例；`supportsServerInfo` 探测）。
- 友好层：`TaskOpKind.serverInfo` ＋ `TaskRuntimeFfi.serverInfo()` → `TaskOpHandle`
  （复用既有 `-3` 自动扩容 wait 循环）。
- `FakeAkTaskCalls`：`serverRevision` 可脚本化（null 默认＝旧服务器降级形），op 立即以
  DONE 收敛（与引擎一致，null 绝不作为失败）。

### Flutter 应用

- UI 契约（`lib/ui/service/simple_transfer_service.dart`）：追加式新成员
  `Future<String?> serverRevision()`（R36-P2-03 `engineHealth` 同一追加先例；契约注明
  null＝“无信息”且永不抛错）。
- 引擎服务（`lib/services/simple_transfer_service.dart`）：`serverRevision()`——runtime 就绪
  才发 op，`_beginOp/_endOp` 计入活动（CR-P2-05 快节奏），一切失败（含旧 DLL
  `UnsupportedError`、超时、租约撤销、disposed）一律降级 null；**不进 phase 状态机**
  （诊断查询绝不动 creating/failed）。
- `FfiSimpleTransferService` 委托；`LocalSimpleTransferService`（fake）加
  `serverRevisionValue` 测试旋钮＋调用计数；`UnusableSimpleTransferService`/`_ThrowingStartService`
  返回 null（无引擎＝诚实“无信息”）；`DiagTaskCallsGate`/`RecordingTaskCalls`/`CountingTaskCalls`
  三个包装 seam 补齐转发。
- **设置页「关于」卡**（`settings_page.dart`）：monospace 显示 revision；null 显示
  “未知（服务器未报告版本）”，查询中显示“查询中…”；带「刷新」按钮。
  **懒激活**：SettingsPage 常驻 IndexedStack（应用启动即 build），查询经 `isActive`
  （`app.dart` 传 `_index == 1`）门控到**首次选中设置页**才发——避免无人查看的启动查询把
  `op_done(task_server_info)` 行塞进共享任务事件队列（首轮实现即被 `app_lifecycle_test`
  的精确队列断言抓到，改为懒激活后修复）。

## 测试（全部实际执行）

| 门槛 | 结果 |
|---|---|
| `cargo test -p ak-ffi`（全量） | ok（新增 `server_info_op_reports_the_registry_revision`、`server_info_op_degrades_and_validates` 2 项，真注册中心往返＋指针纪律/降级） |
| `cargo fmt -p ak-ffi -- --check` | 通过 |
| `cargo clippy -p ak-ffi --all-targets` | 无告警 |
| `python scripts/check_contracts.py` | **5/5 PASS** |
| `flutter analyze`（flutter_app） | No issues found |
| `flutter test`（flutter_app 全量，一次终验） | **171/171 全绿**（含新增设置页「关于」卡 widget 测试：null→未知文案、刷新→版本串、恰好 1+1 次调用） |
| `dart test`＋`dart analyze`（packages/ak_ffi_dart 全包） | **170/170 全绿**；analyze 无问题（新增 3 项：脚本化 revision 往返、null 降级为 DONE 非 failure＋未知 runtime `-4`、导出名不在冻结门列表） |

未执行：真机/桌面 release 构建复验（本轮无平台层改动，沿用 SUPPORT_MATRIX 现状口径）；
`flutter test` 按所有者指示只做了一次终验（避免 flutter_tester 弹窗打扰桌面）。

## 变更清单（18 文件，+657/−10）

Rust：`crates/ak-ffi/src/{lib,task,task_ext}.rs`；契约：`docs/contracts/ffi-v2.md`；
Dart 包：`packages/ak_ffi_dart/lib/src/{task_ffi,fake_task_ffi}.dart`＋`test/task_ffi_test.dart`；
应用：`lib/main.dart`、`lib/ui/app.dart`、`lib/ui/pages/settings_page.dart`、
`lib/ui/service/simple_transfer_service.dart`、`lib/services/{simple_transfer_service,ffi_simple_transfer_service,simple_transfer_fake}.dart`；
测试：`test/{settings,app_shell,diag_stage,ffi_simple_transfer_service}_test.dart`。

## 诚实边界

- revision 的“新鲜度”＝查询时刻的连接世代；重连（换服务器构建）后需「刷新」重查（Rust 侧
  按世代自动失效缓存，重复查询即得新世代答案）。
- 未配置 revision 的生产服务器（`serve_v2` 无参路径）会回答 `"unknown"` 字符串——GUI 原样
  显示；这是服务端 `sanitize_revision` 的既有口径，非本层缺陷。
- 旧 DLL（无 `ak_task_server_info` 导出）：GUI 显示未知，不报错（§8.7 登记的容忍策略）。
- 改动未提交（任务书未要求 commit）；工作树留在 `dev/review-fixes-20261111` 待集成者收口。
