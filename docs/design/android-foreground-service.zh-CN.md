# Android 前台服务设计：长传输的后台存活

> 状态：**设计稿，实现未开始**。本文件对应 [REVIEW_2026-11-11](../REVIEW_2026-11-11.zh-CN.md) CR-P2-05 改法第 3 条："Android 长传输：先出前台服务的设计文档，写清 Android 14 起必须声明的前台服务类型，以及 Android 15 起 `dataSync` 类型的时长限制。先设计，不急于实现。"
> 批次：D（D3）。更新：2026-11-12。
> 口径：第 3 节的 Android 平台行为全部核自 developer.android.com 官方文档（2026-11-12 核对，逐条附链接）；凡未能从官方文档确证的点，正文标注**待核实**，并集中在第 5 节说明。

## 1. 背景

- **引擎在应用进程内。** 传输引擎是 Rust（QUIC 收发、清单校验、落盘全在 `libak_ffi.so` 内完成），经 ak-ffi 加载进 Flutter 应用进程运行（[ARCHITECTURE](../ARCHITECTURE.zh-CN.md)、[ADR-003](../adr/003-ffi-boundary.zh-CN.md)）。Android 端没有独立引擎进程。
- **Android 会回收后台应用。** 应用不可见且没有前台服务时，进程进入 cached 态，是内存压力下的回收对象。当前 [AndroidManifest.xml](../../flutter_app/android/app/src/main/AndroidManifest.xml) 只声明了 `INTERNET`，没有任何前台服务，也没有任何进程存活保障。
- **现状：长传输在后台裸奔。** 用户切后台或锁屏后，长传输随时可能随进程一起被杀；v1 任务码是一次性配对，没有重连/续传机制，进程死亡即任务失败。这正是 CR-P2-05 指出的"长时间传输在 Android 后台很可能被系统终止（没有前台服务）"。
- **D1 只解决了耗电，没解决存活。** D1 已落地的生命周期观察者（[transfer_lifecycle_observer.dart](../../flutter_app/lib/services/transfer_lifecycle_observer.dart)）在 `paused`/`hidden` 时把事件泵降到 1 s 活动节奏、无任务时完全暂停，回前台立即刷新——它降低了后台功耗，但进程本身仍是 cached 态。本设计补的是另一半：让"有活跃任务"期间的进程不被当后台应用对待。
- **时长量级（受益上限的边界）。** 接收确认窗口 30 s（`crates/ak-ffi/src/task.rs:242` 的 `PENDING_CONSENT_WINDOW`）；CR-P2-04 之后 QUIC 显式 `keep_alive` 10 s、`max_idle_timeout` 60 s（[tls.rs](../../crates/ak-net/src/tls.rs) `KEEP_ALIVE_INTERVAL`/`MAX_IDLE_TIMEOUT`）。前台服务的意义是让分钟到小时级的传输熬过"应用不可见"；对这些超时本身没有影响。
- **targetSdk 已显式固定 36**（[build.gradle.kts](../../flutter_app/android/app/build.gradle.kts)，CR-P2-03），高于 34/35，下文 Android 14 与 Android 15 的规则对本应用**全部适用**，不存在"暂未触发"的侥幸。

## 2. 需求

- **R1 存活**：从用户发起/接受传输到任务终态（complete / failed / declined / cancelled），切后台、锁屏不中断。任务终态由引擎事件定义，与 UI 口径同源。
- **R2 可见**：传输期间用户随时能在通知里看到进度（百分比或已传字节、文件计数），与 app 内 UI 同源，不需要另开屏幕。
- **R3 省电**：空闲时**不启动**服务、不持有唤醒；服务生命周期严格等于"存在活跃任务"的区间。D1 已把空闲泵降到 1 s/暂停，服务侧不引入自己的轮询。
- **R4 诚实降级**：服务启动失败、或被系统限时（见 3.2）时，传输按现状继续（不比今天更差），并如实告知用户，不做静默重试。
- **R5 边界不变**：不引入中继（[ADR-001](../adr/001-direct-only.zh-CN.md)）；不改冻结契约（`docs/contracts/`）；Rust 引擎与 FFI 导出零变更——本设计只落在 Kotlin/Dart/清单三层。

## 3. Android 平台约束（均已核实）

### 3.1 Android 14（API 34）起：前台服务必须声明类型

对 targetSdk ≥ 34 的应用（[官方行为变更](https://developer.android.com/about/versions/14/changes/fgs-types-required)、[类型总表](https://developer.android.com/develop/background-work/services/fg-service-types)）：

- 每个前台服务必须在清单声明 `android:foregroundServiceType`，否则调用 `startForeground()` 时系统抛 `MissingForegroundServiceTypeException`。
- 必须声明 `FOREGROUND_SERVICE` 及按类型的权限（`dataSync` → `FOREGROUND_SERVICE_DATA_SYNC`）。这类权限均为 normal 权限：安装时授予、用户不可撤销；缺失时抛 `SecurityException`。
- `dataSync` 无运行时前置条件（不像 camera/location/microphone 需先拿运行时权限）。
- 发布到 Google Play 还需在 Play Console 声明前台服务用途；本项目当前侧载分发，登记为将来上 Play 前的事项。

### 3.2 Android 15（API 35）起：`dataSync` 的 6 小时 / 24 小时限制

对 targetSdk ≥ 35 的应用（[官方行为变更](https://developer.android.com/about/versions/15/behavior-changes-15)）：

- 应用**所有** `dataSync` 服务共享每 24 小时共 6 小时的运行预算；超时后系统调用 `Service.onTimeout(int, int)`（Android 15 新回调），服务须在数秒内 `stopSelf()`，否则系统抛内部异常（`RemoteServiceException: A foreground service of type dataSync did not stop within its timeout`），表现为崩溃。
- 用户把应用带回前台会**重置计时器**；预算耗尽后再启动 `dataSync` 前台服务抛 `ForegroundServiceStartNotAllowedException`。
- 另：targetSdk ≥ 35 禁止从 `BOOT_COMPLETED` 广播启动 `dataSync` 服务。本设计不做开机自启，不受影响。
- 官方提供测试钩子，无需真等 6 小时：`adb shell am compat enable FGS_INTRODUCE_TIME_LIMITS <包名>` 开启限制，`adb shell device_config put activity_manager data_sync_fgs_timeout_duration <毫秒>` 改短时限（见第 6 节）。
- **待核实（小项）**：官方文档未定义"24 小时"窗口的精确语义（滚动窗口还是固定锚点）。不影响设计，只影响提示文案的措辞。

### 3.3 类型选型：为什么选 `dataSync`

| 候选 | 判定 | 理由 |
|---|---|---|
| `dataSync` | **选定** | 官方描述即"数据传输操作（上传/下载、导入/导出……经网络传输数据）"，与设备到设备文件传输完全对口；无运行时前置。代价是吃 3.2 的 6 小时上限。 |
| `connectedDevice` | 备选，不选 | 运行时前置并不高（清单声明 `CHANGE_NETWORK_STATE` 等普通权限之一即可满足，[类型总表](https://developer.android.com/develop/background-work/services/fg-service-types)），但官方意图是"与外设的蓝牙/NFC/IR/USB/网络连接交互"，指向配件类场景。本项目的对端是另一台手机/电脑，属于一般 IP 传输。**为绕开 6 小时上限而借此类型**，在侧载分发阶段没有即刻风险，但 Play 分发需声明类型用途、有被驳回的风险，且平台（如 15 收紧 dataSync）随时可能继续收紧语义。若未来出现 >6 小时常态传输需求，此选项连同 UIDT 一起交所有者拍板（第 5 节）。 |
| `mediaPlayback` | 排除 | 要求真的在播放媒体；与文件传输无关，属不实声明。 |
| `remoteMessaging` | 排除 | 面向短信等消息在设备间接力，非文件。 |
| `specialUse` | 排除 | 兜底类型，需 `PROPERTY_SPECIAL_USE_FGS_SUBTYPE` 写明理由并接受 Play 逐案审查，仅在所有类型都不符时使用。 |
| UIDT（非 FGS 类型） | 演进路线 | [user-initiated data transfer jobs](https://developer.android.com/develop/background-work/background-tasks/uidt)（API 34 引入，`JobInfo.Builder.setUserInitiated(true)` + `RUN_USER_INITIATED_JOBS` 权限）：用户发起、立即执行、须有通知、不受 App Standby 桶配额限制，官方明确把它列为 dataSync FGS 的替代路线（[datasync-migration](https://developer.android.com/about/versions/15/changes/datasync-migration)）。系统仍可能因温度/内存停它，且无 Jetpack 封装。对本案它同样是"进程保活外壳"（引擎同进程），技术上可行；v1 没有 >6 小时需求，本期不用。 |

关于"用户发起的传输"：FGS 类型体系里**没有**为此设的专门类型；官方给"用户发起长传输"的答案是 UIDT（JobScheduler，不是 FGS 类型），其约束模型不同（系统可停、带网络约束），故不作本期外壳。

### 3.4 后台启动前台服务的限制

Android 12（API 31）起，后台应用不能启动前台服务，除非命中豁免（[官方限制页](https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start)）。豁免包括：从用户可见态切出、用户点了与应用相关的 UI 元素（如通知）、FCM 高优先级消息、精确闹钟、`BOOT_COMPLETED`、用户关闭了该应用的电池优化等。

对本案的结论：**两个自然的启动触发点都发生在前台**——发送端点"发送"、接收端在 30 s 窗口内点"接受"。因此设计硬性规定：服务只在应用前台、由这两个用户动作直接触发启动；v1 没有排队/自动重试任务，不存在"回到后台才发现要传"的路径。接收端尤其注意：必须在**接受动作本身**启动服务（此刻必在前台），不能等 QUIC 建连或首包才启动——那时应用可能已经退到后台，豁免不成立。

### 3.5 Doze、App Standby 与电池优化的交互

（[官方功耗文档](https://developer.android.com/training/monitoring-device-state/doze-standby)）

- 前台服务保证进程处于前台服务进程态：不进 cached、不被按 cached 回收；[App Standby 一节明说](https://developer.android.com/training/monitoring-device-state/doze-standby)"应用有前台服务进程在跑"就不算 idle。这是本设计的主诉求，成立。
- 但 **Doze 不因此豁免**：设备未充电、静止、熄屏一段时间后进入 Doze，官方限制清单是挂起网络访问、忽略 wake lock、推迟 JobScheduler/WorkManager 与标准闹钟——该清单没有"有前台服务在跑"这一豁免项。深 Doze 下进行中的 dataSync 传输是否、何时被断网，官方文档没有给出承诺，各 OEM 实现差异大——**待核实**，列为第 6 节实测项。
- 电池优化豁免（用户在设置中手动关闭，或应用请求）是官方认可的部分豁免（可用网络 + 持 partial wake lock），同时也是 3.4 的后台启动豁免之一；但 Play 政策限制"请求直接豁免"仅当核心功能受影响——长后台传输恰好是本项目核心功能，若上 Play 有论证空间。**设计不默认请求豁免**：先用前台服务；若真机实测发现 Doze 截断，再由所有者决定是否加"引导用户豁免"的设置项（第 5 节）。

## 4. 设计

### 4.1 总体形态

手写一个 Kotlin `TransferForegroundService : Service`（清单声明 `foregroundServiceType="dataSync"`），经一条窄 MethodChannel（暂名 `ak.sending/fgs`）由 Dart 侧驱动。服务**只做一件事**：以 dataSync 身份持有前台通知。不做传输、不碰 FFI（Kotlin 不加载 `libak_ffi.so`、不调 `ak_task_*`——Dart 是引擎唯一调用方，保持单一所有权，符合 [ADR-003](../adr/003-ffi-boundary.zh-CN.md) 精神）。不设独立 `android:process`：单进程下引擎与服务同生共死，生命期简单且 FFI 句柄无需跨进程。

### 4.2 插件 vs 自研 MethodChannel + Kotlin Service

| 维度 | pub.dev 前台服务插件（flutter_foreground_service 一类） | 自研 MethodChannel + Service |
|---|---|---|
| 代码量 | 省掉百行级 Kotlin | 约 100–200 行 Kotlin（start/stop/update 三入口 + 通知渠道 + onTimeout） |
| 新口径跟进 | 类型权限、`onTimeout`（API 35）等跟进取决于插件版本与维护节奏，审计面外移 | 直接对齐官方文档，本仓库可控 |
| 语义贴合 | 多为"常驻服务"设计，启停与通知粒度按插件约定 | 启停严格绑定任务活跃区间（R3），通知内容自定义 |
| 依赖策略 | 引入一个运行时依赖及其传递依赖 | 零新依赖 |

**推荐自研**：表面积小、关键交付路径自持，与本仓库受控引入第三方依赖的先例一致（[pubspec.yaml](../../flutter_app/pubspec.yaml) 里每个依赖都附采用理由）；不为三个方法引入一个插件。

### 4.3 启动与停止

- **启动**（均在前台用户动作内，见 3.4）：(a) 接收端用户点"接受"（30 s 窗口内）；(b) 发送端 send-start 提交成功。Dart 在同一动作处理里调用 `channel.start(jobId)`。
- **单实例多任务**：一个服务实例持有"活跃 job 集合"；v1 常见是单任务，多任务并存时通知聚合为"N 个任务，总体 x%"。
- **停止**：job 到达终态（事件来源与 UI 同源）→ `channel.stop(jobId)`；集合清空 → `stopForeground` + `stopSelf`。终态通知转为一条可滑除的完成/失败摘要，不常驻。
- **启动失败**（如 6 小时预算耗尽抛 `ForegroundServiceStartNotAllowedException`）：异常经 channel 回传 Dart，UI 提示"系统已限制后台传输，请保持应用在前台"；传输本身继续（即现状行为），不静默重试（R4）。

### 4.4 通知与进度来源（设计级）

- 通知渠道一条、`IMPORTANCE_LOW`（不出声不弹横幅）；内容：任务方向 + 百分比/已传字节 + "i/N 文件"，`setProgress` 进度条，更新节流 ≤1 次/秒。
- 进度数据两条路线，**服务端（Kotlin）代码不变**，只差 Dart 侧进度怎么来：
  1. **现状（D2 之前）**：D1 的自适应泵在后台仍以 1 s 活动节奏拉 FFI 事件（[simple_transfer_service.dart](../../flutter_app/lib/services/simple_transfer_service.dart)），Dart 每轮拿到进度就 `channel.update` 推给服务。1 s 粒度对通知足够。
  2. **D2 落地后**：Rust 事件原生直推 Dart，转发点相同，无需为通知单做桥。
- 因此本设计**不依赖 D2 的排期**，可先行实现。

### 4.5 与 D1 生命周期观察者的协作

- `paused`/`hidden`：观察者照旧把泵降到 1 s/暂停（UI 侧省电）；同一时刻服务正在生效（进程脱离 cached 态）——两者互补、互不感知，不需要新耦合。
- `resumed`/`inactive`：D1 已实现立即 drain + 刷新；服务侧无动作（通知一直可见）。可选增强：回前台时强制同步一次通知，消除后台期间泵降频造成的展示滞后——影响极小，实现时再看是否值得。
- `detached`：进程将亡，`onDestroy` 兜底清理由系统完成；引擎侧的诚实收尾（waiter 失效等）已属 `dispose()` 既有职责。

### 4.6 Rust 引擎不动

引擎在应用进程内继续跑，前台服务只是改变**进程的系统分类**，不改变线程与内存布局；不需要 NDK 侧服务、不需要引擎改造。FFI 导出与 [ffi-v2 契约](../contracts/ffi-v2.md)零变更。

### 4.7 6 小时限制到达时的行为

- 实现 `Service.onTimeout`（API 35+；更低版本无此回调、也无此限制）：立即 `stopSelf()`，绝不让系统抛 "did not stop within its timeout" 崩溃；经 channel 通知 Dart → 发一条"系统限时到达，后台保护已撤，回到前台可重置"的提示（重置口径见 3.2）。
- 传输不取消：引擎无暂停/让位语义，继续跑退化为现状（无 FGS 保护），若进程随后被杀走既有 failed 路径。取消比继续更差（R4）。
- 不做剩余预算展示：就所查官方文档，系统没有提供查询 dataSync 剩余额度的公开 API（**待核实**，若后续发现再补）。
- >6 小时成为常态需求时的正路是 UIDT（3.3）：届时外壳从 `Service` 换 `JobService`，channel 协议可保持不变。列入第 5 节拍板项，不阻塞本期。

### 4.8 清单与权限变更（设计级示意）

```xml
<uses-permission android:name="android.permission.FOREGROUND_SERVICE"/>
<uses-permission android:name="android.permission.FOREGROUND_SERVICE_DATA_SYNC"/>
<uses-permission android:name="android.permission.POST_NOTIFICATIONS"/>
<!-- ... -->
<service android:name=".TransferForegroundService"
         android:exported="false"
         android:foregroundServiceType="dataSync"/>
```

- `FOREGROUND_SERVICE` 与 `FOREGROUND_SERVICE_DATA_SYNC` 均为 normal 权限，安装即授。minSdk 24 下：`foregroundServiceType` 属性 API 29 引入、`FOREGROUND_SERVICE` 权限 API 28 引入，更早版本忽略相应声明即可，无运行代价。启动统一走 androidx 的 `ServiceCompat.startForeground`（类型位在低版本自动退化为无类型），避免手写版本分支。
- `POST_NOTIFICATIONS`（API 33+ 运行时权限）：[官方明确](https://developer.android.com/develop/ui/views/notifications/notification-permission)——启动前台服务**不需要**此权限，但服务必须附通知；未授予时 FGS 通知不出现在通知抽屉，任务管理器里仍可见，服务照常运行。设计：首次发起/接受传输前请求一次；拒绝不阻塞传输，仅通知不可见（UI 内一次性说明）。
- 需一枚通知小图标（实现时定，可先用现有 launcher 资源裁切）。

## 5. 未决问题与状态声明

- **无阻塞实现的问题**。两处待核实均为实测/文案性质，不影响开工：深 Doze 下 dataSync 服务的实际断网行为（3.5，第 6.4 节实证）；dataSync 24 小时窗口的精确语义（3.2，只影响提示文案）。
- 已登记事项：targetSdk 升 37 前的本地网络权限流程已在 [BACKLOG](../BACKLOG.zh-CN.md) 登记（CR-P2-03），与本设计无冲突——`ACCESS_LOCAL_NETWORK` 是独立的运行时权限流程，不影响 FGS 类型与启停。
- 交所有者拍板（均非阻塞）：(a) 未来若有 >6 小时常态传输，走 UIDT 还是 `connectedDevice` 借类型（3.3 的取舍）；(b) 真机实测若确认 Doze 截断，是否增加"电池优化豁免引导"设置项（3.5 的 Play 政策口径）。
- **明确声明：实现未开始。** 本文件是审核要求的"先出的设计文档"（CR-P2-05 改法第 3 条），任何 Kotlin、清单或 Dart 改动都不在本批次（D3）范围内。

## 6. 实现批次的测试计划（草案，届时并入 [TEST_PLAN](../TEST_PLAN.zh-CN.md)）

1. **服务生命周期**：传输期间 `adb shell dumpsys activity services dev.ak.ak_sending_app` 应显示 `isForeground=true`、type=dataSync（API 34+ 显示 `foregroundServiceType`）；终态后服务消失、通知移除；冷启动空闲时无服务。
2. **后台存活（主验收）**：发起传输 → Home 键 / 锁屏 → 传输完成且双端字节一致；与当前无 FGS 的版本在同一机型上对照（预期部分 OEM 会杀后台进程），差异作为修复证据记录。
3. **前台启动约束**：确认两个触发点（发送、接受）都只在前台调用 `start`；不存在后台启动路径（设计约束自查，无需专门设备测试）。
4. **Doze**：传输进行中 `adb shell dumpsys deviceidle force-idle` 强制进入 Doze，观察是否断流（记录机型与系统版本），`unforce` + `dumpsys battery reset` 恢复。此即 3.5 待核实项的实证。
5. **6 小时限时（不真等 6 小时）**：在 Android 15+ 设备/模拟器上 `adb shell am compat enable FGS_INTRODUCE_TIME_LIMITS dev.ak.ak_sending_app`，再 `adb shell device_config put activity_manager data_sync_fgs_timeout_duration 60000` 缩到 1 分钟，验证 `onTimeout` 路径：服务自停、无崩溃、提示正确、传输退化为无保护继续。
6. **通知权限**：`adb shell pm revoke dev.ak.ak_sending_app android.permission.POST_NOTIFICATIONS` 后发起传输——服务仍运行、任务管理器可见、抽屉无通知；`pm grant` 后恢复。
7. **回归**：服务层 Dart 单测（fake channel 覆盖 start/stop/update/启动失败转发）；`flutter analyze` + `flutter test`；清单变更后 `flutter build apk --debug` 与 `scripts/build_android_ffi.sh` 照常；`python scripts/check_repository.py`。

## 参考资料（均于 2026-11-12 核对）

- [Android 14 行为变更：必须声明前台服务类型](https://developer.android.com/about/versions/14/changes/fgs-types-required)
- [前台服务类型总表（各类型权限与前置条件）](https://developer.android.com/develop/background-work/services/fg-service-types)
- [Android 15 行为变更：dataSync 超时（6 小时/24 小时）](https://developer.android.com/about/versions/15/behavior-changes-15)
- [dataSync 前台服务的替代方案（含 UIDT）](https://developer.android.com/about/versions/15/changes/datasync-migration)
- [用户发起数据传输作业（UIDT）](https://developer.android.com/develop/background-work/background-tasks/uidt)
- [前台服务后台启动限制与豁免](https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start)
- [Doze 与 App Standby](https://developer.android.com/training/monitoring-device-state/doze-standby)
- [POST_NOTIFICATIONS 运行时权限与前台服务](https://developer.android.com/develop/ui/views/notifications/notification-permission)
