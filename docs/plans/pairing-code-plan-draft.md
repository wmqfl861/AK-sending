已完成只读审阅：`flutter_app/lib/services` 全部五个文件、`lib/ui/service/transfer_service.dart`、`ak_ffi_dart/lib` 全部绑定/模型/状态源码，以及配对页、Fake 服务、相关测试入口与主要断言、Windows/Android 构建配置、跨目标检查脚本、CI 和第十七波证据。未修改文件，未执行构建或测试；下面“已验证”仅指仓库已有历史证据。

**1. 当前真实链路与需要写进总计划的边界**

| 事实 | 代码依据 | 计划影响 |
|---|---|---|
| UI 实际依赖 `lib/ui/service/transfer_service.dart`，由 `FfiTransferService` 实现，再调用 `RealTransferService` 与 `AkEngineDart` | `flutter_app/lib/ui/service/transfer_service.dart:241`；`lib/services/ffi_transfer_service.dart:103` | 不要把另一个同名的低层 `lib/services/transfer_service.dart` 当成页面契约。新配对服务、队列事件必须沿这条链交付、Fake 同步实现 |
| `RealTransferService` 负责 FFI ID 注册表、事件轮询、按 job ID 路由，50ms 一轮、每轮至多 512 条 | `lib/services/real_transfer_service.dart:100`、`:116`、`:225` | Rust 保持真实调度器权威；Dart 只做呈现与调用。扩展时保证轮询有界、终态可查询，进度高频合并，防 UI 长期落后 |
| 上层当前仅一个 `_receive`，发送立刻启动，无排队策略 | `lib/services/ffi_transfer_service.dart:270`、`:428`、`:495` | 新需求不能通过增加两个 UI 开关就完成；须新增共享 Rust 调度层与常驻接收服务，再以多个独立 task/controller 呈现 |
| `_sends` 超过 16 条直接删最早记录，无论是否活动 | `lib/services/ffi_transfer_service.dart:274`、`:522` | 新多任务功能必须改为“所有活动任务保留＋仅已结束历史有界淘汰”，否则第 17 个任务会使仍在运行任务失去事件/结果路由 |
| `stopReceive()` 当前取消唯一接收 job，等待最多 5 秒，再清 `_receive` | `lib/services/ffi_transfer_service.dart:471` | 新契约须区分 `stopListener`、`cancelTask(task_id)`、`disconnectChannel(channel_id)`、关闭整个服务；不能沿用“停止接收＝杀全部任务”的隐含语义 |
| 当前 facade 与 core 默认各自打开一个 native engine | `lib/services/ffi_transfer_service.dart:113`；`lib/services/ak_engine_facade.dart:86` | 邀请监听和队列不能分别落在两个独立 runtime。建议每身份目录一个应用级 native runtime，facade 只借用；需调整所有权 |
| facade 支持注入 engine，但 `dispose()` 仍无条件释放它 | `lib/services/ak_engine_facade.dart:141` | 单实例接线不能只注入一个字段；应由 runtime owner 统一 dispose，borrowed facade 不释放 |
| 同一个 FFI 句柄调用必须串行，不能跨 isolate 共享 | `packages/ak_ffi_dart/lib/src/engine.dart:9`；`lib/services/real_transfer_service.dart:8` | 配对网络请求必须 native async start/poll/result；任何同步 ABI 只能解析/入队/读取快照，不得等待 TCP、TLS 或对端上线。可保持单 Dart isolate，若后移到专用 isolate，只传 DTO/消息，句柄唯一归属 |
| `peerList/Add/Remove` 已真实接线 | `packages/ak_ffi_dart/lib/src/engine.dart:212`、`:235`、`:270`；`lib/services/ak_engine_facade.dart:125` | 新自动配对应复用 identity 的统一原子落簿实现，不能在 Dart 手拼 `peers.book` |
| Android identity dir 异步解析；identity/peer/history 方法等待，但同步 startSend/startReceive 本身不能 await | `lib/services/ffi_transfer_service.dart:136`、`:296`、`:428`、`:495` | 新服务必须在 `identityDirReady` 后才能启动 pairing runtime/常驻 listener；未就绪禁用入口或使新 `Future` 启动接口明确 await，避免把长期凭证写到临时目录 |

**2. 第十七波之后尚存的 Dart 契约漂移应作为第十八波基线清理**

- `AkReceiveSpec` 仍将 `expect` 定义为可省略，注释写默认 `any`，`toJson()` 只有非 null 才输出：`packages/ak_ffi_dart/lib/src/ffi_json.dart:329`、`:346`。Rust 已强制必填，因此应改成 `required this.expect`，默认页面仍显式传 `book`。这属于同步已有安全契约，不能用新功能顺便恢复隐式 any。
- Dart `_checkString()` 只拒绝 `< U+0020`：`ffi_json.dart:78`；应与 Rust 的 C0/DEL/C1 完全一致。新增 code/label/server 配置和设备名称使用同一规则。
- `AcceptPolicy.parse` 只识别 any/指纹/六位短码，未包含 `book`；`ffi_json` 又包含 book。应统一命名及模型，但不要把新邀请码放进 `expect` 的六位分支。
- `JobRecordJson.id` 注释仍说 wire job ID：`lib/src/identity.dart:89`；真实 DLL 测试已明确 record ID 不等于 wire job ID：`test/ffi_bindings_test.dart:1038`、`:1052`。新设计必须明确 `invitation_id`、`channel_id`、本地 task ID、FFI operation ID、wire content job ID、checkpoint record ID 的不同角色。
- 当前 `IdentityInfo.pairingCode` 是旧六位指纹派生值，并且 `toString()` 输出它；可兼容保留 JSON `pairing_code`，在新的 Dart/UI 类型中改用 `fingerprintVerificationCode`/“指纹校验短码”。新长期凭证使用 `invitationCode`/“配对邀请码”，独立 DTO；其 `toString`、日志、错误文本都脱敏，复制后的 Snackbar 只写“已复制邀请码”。
- 当前配对页文案“对端在发送页输入此配对码或完整指纹即可配对；发送成功自动记入本机配对簿”与当前 GUI 发送服务只收完整 64 hex 不一致：`lib/ui/pages/pairing_page.dart:291`，`lib/services/ffi_transfer_service.dart:497`。新页交付时必须删除这类过时路径说明，以实际双侧落簿事件为准。

**3. 建议冻结的前端/绑定契约**

建议页面契约新增独立的 `PairingService`，避免把所有网络操作塞进旧 peer CRUD；仍由同一个应用 runtime 实现，页面依赖接口，Fake 可独立开发。

```dart
abstract interface class PairingService {
  Future<void> start(PairingServiceOptions options);
  Future<InvitationCreated> createInvitation(CreateInvitationOptions options);
  Future<RedemptionHandle> redeemInvitation(String invitationCode);
  Future<InvitationPage> listInvitations({String? cursor, int limit = 32});
  Future<void> revokeInvitation(String invitationId);
  Stream<PairingEvent> get events;
  Future<void> stop();
}

abstract interface class TransferSchedulerService {
  Future<ScheduledTask> enqueue(ScheduledSendOptions options);
  Future<void> cancelTask(String taskId);
  Future<void> configure(SchedulerPolicy policy);
  Stream<SchedulerEvent> get events;
}
```

字段建议与 root 其他模块统一后再冻结：

- `InvitationSummary`：`invitation_id`、`created_unix_ms`、`expires_unix_ms`（null＝长期）、`state`、本地 label、可选 masked suffix；列表不返回明文码。
- `InvitationCreated`：上述摘要＋仅创建时显示的 `invitation_code`。若产品要重显，必须明确安全存储契约，不能直接塞进 SharedPreferences、普通 JSON 配置或历史。
- `PairingEvent`：固定枚举 `service_online/service_offline/redeem_pending/peer_prepared/local_book_committed/pairing_complete/redeem_failed/invitation_revoked`；固定 operation/session/invitation/peer 标识，不携带任意字段透传。
- `SchedulerPolicy`：建议 `same_code_mode: "serial"|"parallel"`、`different_codes_mode: "parallel"|"serial"`、`max_active_tasks_per_code`、`max_active_code_connections`、`max_queued_tasks`。UI 展示为“同一码的任务：排队/同时传输”和“不同码的连接：同时连接/依次连接”。
- `ScheduledSendOptions` 通过 `channel_id`/`invitation_id` 的已授权映射选择对端，不能要求 UI 从服务端消息提取候选后自行拼 `fp_hex`、绕开认证与路径闸门。
- 不把邀请码等同于配对设备：同一码可被多个设备使用时，任务需要明确的 recipient/channel ID；页面选择目标设备，不能默默广播。
- UI 状态除当前 `idle/connecting/transferring/verifying/done/failed` 外，应新增独立 scheduler 状态 `queued/waiting_peer/waiting_direct_path/waiting_capacity/cancelling/cancelled`。不要通过按 enum.index 单调前进实现重试状态；现有 `JobController` 的连接内相位可继续保持单调，新调度控制器包住它。

C ABI 应保持 ADR-003：新增短时 start 导出产生 16B operation/session ID，结果/事件轮询写调用方缓冲区，保留 `-3` 无消费/无重复副作用和现有错误码。可以使用专门 `ak_pairing_poll_event`；若复用 `ak_poll_progress`，必须先冻结 `kind` 分流，不能直接塞没有 `job` 的配对事件，因为当前 `ProgressEvent.fromJson()` 强制每条事件有 `job`，会将其丢为 malformed。列表分页且有页大小上限，不能依赖现有 16MiB 自动扩容无限吸收列表。

创建/兑换操作要有 `request_id` 幂等，缓冲不足后重试只能取同一结果，不能再生成一个码、再落一遍簿。取消正在兑换的 operation 与撤销长期码是两个不同操作。

**4. 前端任务拆分与互斥所有权**

| 执行者 | 唯一所有权目录 | 工作 | 上游依赖 | 验证门槛 |
|---|---|---|---|---|
| Dart 绑定 | `flutter_app/packages/ak_ffi_dart/` | 新 symbols、typed DTO、异步 operation 包装、pairing/scheduler 状态；修复 expect/Cc/record_id 契约漂移 | 第十八波 FFI 签名/JSON 冻结即可写，Fake 先行；真实 ABI 后接力 | `dart analyze`、`dart test`；新增真实 DLL start/poll/result/取消/-3/重启恢复测试 |
| 服务集成 | `flutter_app/lib/services/` | 单 native runtime 所有权；配对/调度真实适配；身份初始化等待；活动任务注册表修复；事件公平轮询 | 冻结 UI 接口和 Dart API，可 Fake 并行；Rust/Dart 交付后接线 | 服务 Fake 契约测试＋真实 DLL seam；>16 活动任务完整路由；dispose 仅一次；身份目录切换受控 |
| UI 契约/页面 | `flutter_app/lib/ui/` | 新邀请码页/现有配对页整合；两个模式开关；按码/连接分组队列；长期码列表/撤销；状态文案 | 冻结接口后可全并行 Fake 开发 | widget tests：复制脱敏、错误不枚举、离线等待、两侧提交前不显示完成、撤销与移除区别、模式切换不虚假完成 |
| Flutter 测试集成 | `flutter_app/test/` 或单独 `integration_test/` owner | Fake 接口同步、真实窗口 e2e；与 UI 执行者不编辑同一测试目录 | UI 和服务契约先行 | `flutter analyze`、`flutter test`；真实 GUI→真实 Rust→真实本地 signal-server |
| Linux runner | 新 `flutter_app/linux/` | runner/CMake、`libak_ffi.so` 打包和 RPATH、插件注册、系统依赖文档 | Dart ABI 冻结即可并行 | Windows 上只能静态核对/文档；Linux runner 上实际 Flutter build/test/运行才算完成 |
| Android 原生 | `flutter_app/android/` | 必要的前台/恢复生命周期（如纳入首版）；双 ABI so 刷新 | native ABI 后接力 | APK 构建＋包内 symbols/hash；模拟器验证单列，物理真机另列 |
| 构建矩阵 | `scripts/` | 补全所有新旧 symbols 检查、Windows/Linux/Android 打包矩阵 | ABI 冻结即可并行 | 脚本自检、Windows/Android 实际构建、Linux check 的 PASS/BLOCKED 明确区分 |
| CI | `.github/` | Linux Rust/Dart/Flutter 构建 job 与 release deep tests | 依赖脚本与 runner | YAML 静态检查≠CI执行；未授权推送时不得假报 Actions 成功 |

同一执行者要覆盖另一个目录时应分接力任务并移交所有权；例如 UI 作者先完成 `lib/ui/`，下一棒测试作者独占 `test/`，避免 UI 与服务作者共同改 Fake 文件。

**5. 页面与运行生命周期的关键产品语义**

- 发起方无角色限制：发送页和接收页都能进入“生成邀请码”或“输入邀请码”；生成时不自动提交任何文件任务。
- “长期有效”不等于 Android 进程永远在线。服务被 OS 挂起时，另一端显示等待对端上线；恢复后重连和同步持久配对事务。若要后台自动配对，必须交付原生 foreground service 和对应 Android 版本实际验收，不能靠 Dart Timer 声称后台可用。
- 邀请码撤销阻止后续兑换；已完成配对不会自动从对方设备删除。UI 必须区分“撤销邀请码”“移除配对设备”“取消当前任务”，总计划应明确是否另有批量断连/禁入策略。
- GUI 切换并行→串行应显示“现有任务完成后生效/正在收敛”，不突然杀正在写文件的传输。后端发回有效策略与配置 revision，页面不能只依据本地开关立即宣称并发数已经下降。
- 主机或账号切换时禁止混用已开启 signal session、queue、旧 identity dir；停止旧 runtime、清理凭证显示后再启动新目录。
- 共享码队列的实际调度必须跨 CLI/GUI 复用 Rust 权威；如果 daemon 与 GUI 可共用身份目录，必须有单实例所有权/IPC 或排他锁，否则两个本地进程各自的“串行”总和仍会并行。

**6. 现有证据与平台验收矩阵**

- `reports/dev-validation-2026-11-02.txt:447` 起记录第十七波闭环：Rust **507/0**、Dart **85/85**、Flutter **84/84**、仓库检查通过；5GiB release 回归也有独立记录。本轮没有重跑。
- Windows 已有 runner 和 DLL 随包配置：`flutter_app/windows/runner/CMakeLists.txt:44`。计划门槛应是 Rust release DLL/CLI＋`flutter build windows --release`＋真实窗口邀请码 e2e，确认加载的是当前构建 DLL。
- Android 已有 `x86_64`、`arm64-v8a` so，`minSdk=24`：`android/app/build.gradle.kts:22`，主 manifest `INTERNET`：`:6`。现有脚本 `scripts/build_android_ffi.sh:184` 只抽查 `ak_engine_new ak_identity_info ak_jobs_list`，新计划必须把 `ak_peer_*` 与新 pair/scheduler exports 全纳入符号验收。
- Android→宿主有模拟器跨网络栈 CLI 证据，不能算物理双机。反向经 UDP 垫片的记录已按 INV-008 重新归类，不能用来证明直连。
- **`flutter_app/linux/` 当前不存在。** `defaultAkDllFileName()` 已能返回 `libak_ffi.so`，但这不等于已有 Linux runner/打包。
- `scripts/check_targets.sh` 明确 Windows 全 Linux check 可能因 ring/blake3 的 C 依赖编译器缺失为 **BLOCKED**，只可将 `ak-core/ak-wire` 的纯 Rust 子集记为通过，不能外推整个 workspace。
- `.github/workflows/ci.yml` 顶部明确还未在 Actions 运行。当前存在 Ubuntu Rust/portability job 仅是配置证据，不是 Linux 测试证据。
- 首版 Linux 验收应预留真实 Linux runner：`cargo build --release -p ak-cli -p ak-ffi`，workspace/default+sqlite tests、clippy、Dart test、Flutter Linux release build、动态库装载、signal-server 持久化重启、GUI/CLI 双端传输。Windows 本机只能做能完成的 cross-check 与完整构建文档，余项标未执行。

**7. 必须增加的 Dart/Flutter 回归场景**

1. 新邀请码与旧六位短码在解析、展示、复制、错误提示中不能混入同一路径；新邀请码不能被作为 `expect` 传递。
2. 缺失 expect、DEL/C1、未知模式、错误 ID、超限页面大小、错误 duration 在 Dart 与 Rust 两端均拒绝。
3. 并发创建/重复提交/网络重试，前端只出现一个 invitation/op；`-3` 不重复创建、兑换、撤销。
4. 一端已落簿、另一端离线/写入失败：界面明确待完成，重连后幂等完成；坏簿不可渲染成空簿后覆盖。
5. 多码＋多任务时事件按不可混淆 ID 路由；**至少 17 个同时活动任务**，验证不会因原来的 16 条历史截断丢控制。
6. 所有四种两维模式组合、切换收敛、队列满、取消排队项、取消活动项、取消单连接均不影响无关任务。
7. 单 engine 生命周期、两套 facade/服务共同使用、dispose 幂等、取消后端口立即可复用。
8. Android identity dir 未 ready 时不能启动；旋转/后台/进程重建后只恢复有效 metadata、重做真实连接授权。
9. 真实 DLL 测试延续既有 `peerList` 缺簿零写、坏簿不覆盖、upsert 保时间、unknown remove -4、C ABI buffer -3 无损；新增并发自动落簿与手工 add/remove 竞态。
10. UI Fake 成功不计真实配对验收；最终以真实 server＋两个真实 native runtimes 的双侧 `peers.book` 与实际传输字节一致为准。