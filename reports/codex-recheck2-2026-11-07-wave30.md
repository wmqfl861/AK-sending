# AK-sending 第三十波修复第二轮复核报告

评审日期：2026-11-07。评审对象：`5dade176c5845c555f48400107ca987d8629f297`；差异基线：`77d95597cf0731f8f48f0a492f092942aaf3e7b8`。上一轮依据：[wave-29 复核报告](codex-recheck-2026-11-07-wave29.md)；原始发现依据：[wave-28 终审报告](codex-review-2026-11-06-wave28.md)。下列源码行号均对应本次评审提交。

本次依照仓库并行规则，分别只读审查会话库、注册服务器及契约、Dart/Flutter，由集成评审核对 FFI、引擎和跨层时序。已阅读 README、HANDOFF、ROADMAP、ADR-001、上一轮报告、相关实现、修复差异及测试源码。开始时 HEAD 为上述提交、工作树干净；本次唯一写入是本报告。**没有执行任何构建、测试、仓库检查、语法检查、设备实验、网络实验或部署操作。**

判定口径延续上一轮：逐一核对原触发、上轮剩余范围及本轮完整修复声明。“已关闭”仅表示源码关闭了相应路径，不表示测试运行通过、真机验收通过或已提交二进制与源码对应关系已经验证。原主要场景已经修复、但明确声明的分支或原剩余范围仍有缺口，判为“部分”。旧编号中的 P1/P2 是历史分组，不表示每个剩余问题仍具有原严重度。原项与新增根因交叉引用，不把关联条目当作彼此独立的缺陷数。

## 逐项复核表

### 原 P1 发现：6 项

| 编号与上轮剩余范围 | 上轮判定 | 本轮判定 | 修复证据与剩余范围 |
|---|---|---|---|
| **P1-01：真实 GUI 入口接线** | 已关闭 | **已关闭** | `flutter_app/lib/main.dart:85`、`:100` 保持真实 facade → borrowed engine → task bindings → `FfiSimpleTransferService` 链；Android 身份目录等待保留于 `flutter_app/lib/services/ffi_simple_transfer_service.dart:180`。生产入口没有回退为 Fake。新接收等待释放问题另计 **R30-P1-01**，不重新打开原“main 无条件选 Fake”的触发。 |
| **P1-02：ffi-v2 四处 AKT1 示例残留** | 部分 | **已关闭** | `docs/contracts/ffi-v2.md:398`、`:410`、`:426`、`:445` 的 join、create 结果、connected 事件和 list 示例均已改为真实 AKI1 形状；该契约中不再有 AKT1。UI 的 AKI1 前缀、32 个 Crockford 符号及规范化规则继续保留。此结论针对上轮剩余的码格式一致性。 |
| **P1-03：单次额度与竞争败者错误送达** | 部分 | **部分** | `crates/ak-signal/src/server_v2/store.rs:980` 的 Immediate 事务及 `:1048` 的提交数复查保持有效；`:1054` 满额转 aborted，原越额不变量已经关闭。本版服务端 `engine.rs:1867` 改发 `Pairing/pairing_id`，本版客户端能够及时处理，原同版丢帧路径也已关闭。但本轮同时声明的 **Code 域状态查询归因**仍把 `Ok(Aborted)` 判为不属于自己，完整客户端分支声明尚未成立，详见 **R29-P2-01**。这里保留“部分”不表示本版还允许多次 Commit，也不表示本版 Pairing 域竞争错误仍被丢弃。 |
| **P1-04：单次握手失败终结整个 listener** | 部分 | **已关闭** | `crates/ak-ffi/src/task.rs:2721` 的 accept 循环对 Unauthorized 继续，对 Transport/PeerIdentity 等错误在 `:2740` 写入固定词表事件后继续；`:2724` 仅把本地 endpoint closed 的 NotConnected 分支视为监听结束。此分支与 `crates/ak-net/src/lib.rs:527` 的实际错误来源吻合。已配对真实指纹准入仍在 `task.rs:2715`，Offer 归属复核仍在 `:787`。`task.rs:5409` 附近新增 wrong-pin 后合法发送测试源码，覆盖原缺口；本次未执行。新事件文档形状不一致另计 **R30-P3-01**。 |
| **P1-05：生产 FFI 未安装 Commit 前门禁** | 未关闭 | **已关闭** | `crates/ak-ffi/src/task.rs:1235` 明确先调用 `install_redeem_offer_gate()`，随后 `:1275` 才启动 `spawn_owner_loop_verified`。`crates/ak-session/src/pairing/task.rs:986` 检查 endpoint 存在，`:989` 解码，`:992` 检查互补方向；`orchestrator.rs:803` 拒绝后 abort 并 continue，未进入 `:825` 的 owner 配对流程。原生产绕过门禁、错误兑换先 Commit 再失败的路径关闭。须限定声明：FFI `task.rs:2842` 仍推断/默认 Receiver；无角色加入 Sender 创建的码可以合法配对。新增生产测试 `:5278` 的无角色负例只针对 Receiver 创建方，不能改述成“所有无角色请求都会拒绝”，也不能代替缺 endpoint 的负例。 |
| **P1-06：同一个 connections 流的第二次 listen 无快照** | 部分 | **已关闭** | `flutter_app/lib/services/simple_transfer_service.dart:481` 改用 `Stream.multi`；每次 listen 在 `:486` 独立订阅 live，再在 `:495` 发送当前快照，`:497` 仅取消本订阅。`flutter_app/test/simple_transfer_service_test.dart:278` 起的测试保存同一个 stream，`:291`、`:293` 对其分别 listen，检查第二订阅快照及第一订阅取消后的继续更新。本次只读测试源码。 |

### 原 P2 发现：15 项

| 编号与上轮剩余范围 | 上轮判定 | 本轮判定 | 修复证据与剩余范围 |
|---|---|---|---|
| **P2-01：request_id 受理后 op 暂不可查询** | 部分 | **已关闭** | `crates/ak-ffi/src/task.rs:1827` 持 request registry 锁，`:1843` 创建 Running entry，`:1857` 先插入 ops，再于 `:1859` 发布 request_id → op_id；重放者获取 registry 锁时 entry 已存在。`:1903` launcher 复用相同 entry，不二次创建。原操作域、conn 绑定及终态后重放保留；见 **R29-P2-03**。 |
| **P2-02：并发首次初始化重复登录** | 已关闭 | **已关闭** | `crates/ak-ffi/src/task.rs:1170` 的 async 初始化锁、`:1171` 锁内二次查栈、`:1287` 安装前 stopped 检查继续有效。删除 watcher 后，未安装栈仍在 `:1299` 终止 owner/pump 并关闭 client。没有恢复旧的并发重复登录时序。 |
| **P2-03：空闲断开通知缺接收方→纯发送方方向** | 部分 | **部分** | `crates/ak-ffi/src/task.rs:2337` 仍只通过对端 QUIC 数据 listener 发送 TASK-CLOSE；Sender 创建/加入分支仍不绑定该 listener，见 `:2486`、`:2868`。接收方主动断开时，空闲纯发送方仍要等下次发送才发现。`docs/contracts/ffi-v2.md:461` 也保留这个限制。本轮没有关闭上轮明确剩余的另一方向。 |
| **P2-04：GUI 未撤销未加入码；TTL 本地回收和撤码确认不足** | 部分 | **部分** | GUI 接线已补：`flutter_app/lib/services/simple_transfer_service.dart:405` 保留 code_id；adapter `ffi_simple_transfer_service.dart:216` 向上返回；controller `pairing_flow_controller.dart:159`、`:171` 调 `disconnect(codeId)`；UI `home_page.dart:503` 已接取消。`home_flow_test.dart:250` 起有 Fake 参数断言。但 native `crates/ak-ffi/src/task.rs:2310` 仍异步撤码并丢弃结果，随后先发 code_cancelled、标记 cancelled；注册失败不可从 GUI 的调用成功判断。`crates/ak-session/src/pairing/task.rs:668` 的 ActiveTask 仍不保存到期时间，未增加 TTL 驱动的本地 listener/echo/缓存回收。600 秒注册 TTL 与显式本地取消有实质改善，不能等价为“所有到期资源回收、服务器撤销确认完成”。 |
| **P2-05：完成后 UI 仍保留失效发送入口** | 部分 | **已关闭** | native `crates/ak-ffi/src/task.rs:2776` 发 done，`:2778` 置 disconnected/done；Dart `flutter_app/lib/services/simple_transfer_service.dart:820` 的终态表包括 done，`:938` 的 TaskDone 本身收敛行，`:961` 也处理 disconnected/done。adapter `ffi_simple_transfer_service.dart:236` 过滤 done/disconnected，详情页在行消失后撤掉发送入口。拒绝后 native `task.rs:2782` 继续监听的分支保留。 |
| **P2-06：同意期间保存目录与实际落盘目标不一致** | 部分 | **部分** | saveDir 已从事件贯通到 UI，等待与完成横幅均优先显示请求字段，见 `task_events.dart:93`、`simple_transfer_service.dart:933`、`ffi_simple_transfer_service.dart:249`、`incoming_requests_host.dart:188`、`:234`。但 native `crates/ak-ffi/src/task.rs:2750` 在 TLS accept 后冻结 ReceiveSpec 的目录，`:823` 到 Offer 时又独立读取全局 save_dir 作为事件字段。两次读取间 A→B 会造成“事件/UI 为 B、实际落盘 A”。pending consent 内改设置也不会在同意时重新解析。本版既未完成“同意时解析”，也未完整实现“绑定并展示同一个不可变实际目标”的替代方案；详见下面的一致性说明。 |
| **P2-07：同步 consent 阻塞 Tokio worker** | 已关闭 | **已关闭** | `crates/ak-ffi/src/task.rs:794` 登记 oneshot 后返回 Pending，定时器在 `:839` 异步等待；`crates/ak-engine/src/lib.rs:3552` await 决定。没有重新引入 Condvar 或 worker 内同步等待。本轮请求终态顺序问题另归 P2-11 / **R30-P2-03**，不意味着旧 worker 饥饿重新出现。 |
| **P2-08：注册/协调断线误杀健康直连任务** | 未关闭 | **已关闭** | session 的 `spawn_network_watcher` 实现及 FFI 生产安装均删除。`crates/ak-ffi/src/task.rs:1272` 只启动 owner loop 和任务事件泵；`:1316` 不订阅 registry 连接状态。session `task.rs:940` 保留的是供数据面调用的 report_connection_lost。native、Dart、ffi-v2 四个断开理由已统一为数据面终态。`crates/ak-ffi/src/task.rs:5340` 的新增测试源码通过两个真实 FFI engine 配对、关注册服务、再发送，覆盖旧 Fake 字符串测试遗漏的路径；未运行该测试。 |
| **P2-09：native 无条件把失败探测收敛为 connected** | 部分 | **已关闭** | `crates/ak-ffi/src/task.rs:1419` 的 settle_probe_state 依据真实 ProbeOutcome 选择 Connected/Unreachable，仅修改 Probing；owner `:1409`、joiner `:2954` 均调用。list 的状态输出、Dart 的 true/false/null 归并及 `ffi-v2.md:444` 的 unreachable 已一致。此探测只是 UDP 候选可达性，不是身份或可信路径证明；Unreachable 仍允许后续尝试真实 QUIC，不构成绕过身份授权的证据。 |
| **P2-10：固定总时限误断健康长传输、取消失败却声称成功** | 部分 | **部分** | 代码确实加入按进度续期，并在取消抛错时显示“可能仍在后台进行”，见 `flutter_app/lib/services/simple_transfer_service.dart:547`、`:555` 与 adapter `ffi_simple_transfer_service.dart:285`、`:287`。但发送续期丢掉 pump 已读取的进度，加入侧接收又缺 code_id 关联，健康长任务仍会误取消，分别见 **R30-P2-01/02**。新的接收轮询还引入释放后访问 **R30-P1-01**。成功文案依然以 disconnect 返回作为依据，没有新增原生 abort 已实际完成的确认协议，不能把“请求取消成功”扩大为停止完成确认。 |
| **P2-11：原生到期/关闭与横幅生命周期不同步** | 部分 | **部分** | `crates/ak-ffi/src/task.rs:847` 与 Dart `task_events.dart:108`、service `simple_transfer_service.dart:964` 的 consent_expired 名称已对齐，本地镜像 Timer 已删除。正常有序断开也可由 `disconnected → _markTerminal → _expirePendingJobsOf` 关闭横幅。但 native `request_closed` 没有 Dart 解析/处理，迟到 incoming 不检查连接或请求终态；终态先于 incoming 的真实线程交错会留下永久待同意横幅，见 **R30-P2-03**。不能据普通有序到期/断开测试判定所有请求终态已经贯通。 |
| **P2-12：UDP echo 响应互相应答** | 已关闭 | **已关闭** | `crates/ak-ffi/src/task.rs:1560` 继续忽略全零 nonce 的回复，`:1587` 的响应继续使用该标记，原反馈环未恢复。每来源预算和有限来源表仍在；这不等于已验证全局抗洪泛，也不等于 UDP echo 已完成密码学身份认证。 |
| **P2-13：v1→v2 schema 迁移非原子** | 已关闭 | **已关闭** | `crates/ak-signal/src/server_v2/store.rs:1342` 仍以单个 Immediate 事务包含查列、补列、设置 user_version 及 `:1355` 的 commit；原部分迁移兼容分支保留。wave-30 未破坏既有事务保护。没有执行迁移或故障注入。 |
| **P2-14：wave-28 的 41B claim journal 无法幂等重放** | 部分 | **已关闭** | `crates/ak-signal/src/server_v2/store.rs:483` 使用归一比较；`:1556` 仅把“长度 41 且尾 5 字节全零”的默认扩展折为 36B 前缀。非默认 cap/presence 不折叠，TTL、digest、endpoint 差异继续冲突，旧磁盘行不重写。参见 **R29-P2-02**。 |
| **P2-15：endpoint 缓存历史失败无界积存** | 已关闭 | **已关闭** | `crates/ak-session/src/pairing/client.rs:166` 的 256 项上限、`:227` 的 FIFO 淘汰、`:238` 的 Pairing 错误清理、`:355` 的断线清理及 `:565` 的消费清理保留。原有“不是每个终态立即回收、可能淘汰活动提示”的边界没有变成本波已证实的新故障，缓存也未重新无界。 |

### 上一轮新增 R29 发现：5 项

| 编号与上轮剩余范围 | 本轮判定 | 修复证据与剩余范围 |
|---|---|---|
| **R29-P2-01：Commit 满额错误无法进入真实客户端业务流** | **部分** | **本版客户端＋本版服务器的原触发已关闭。** `crates/ak-signal/src/server_v2/engine.rs:1867` 的 0x2109 改为 Pairing/败者 pairing_id，`:1874`、`:1875` 向双方发送；客户端 `client.rs:277` 转入对应 inbox/plane，`orchestrator.rs:1596` 按 pairing_id 失败。`protocol-v2.md:266` 与 `pairing-flow.md:84` 已同步，帧结构未变。但本轮另加的 Code 扇出及归因分支并不成立：`orchestrator.rs:1621` 只把 status query 的 `Err(ClientError::Pair)` 认作本 flow 失败，而 Commit 败者在 `store.rs:1054` 被保留为 aborted，`engine.rs:2033`、`:2047` 正常返回 `Ok(PairStatusReply { state: Aborted })`。因此连接仍发送 Code 域竞争错误的 wave-29 服务端时，归因返回 false，继续等待超时。本轮“Code 域＋状态查询使败者及时终态”的完整声明不能关闭；不另计新的独立发现。 |
| **R29-P2-02：36B 恢复编码破坏历史 41B 默认记录** | **已关闭** | `store.rs:1556` 的窄范围归一精确覆盖 wave-28 的默认体，未泛化为只比前缀。`store.rs:3208` 起的测试构造历史 41B 行、重开并重放，检查不改磁盘及参数冲突；`:3312` 起覆盖 41B 默认、36B 默认及非默认混存。fixture 与 wave-28 实际无条件写扩展尾的编码形状相符。这里只确认测试源码覆盖范围，未运行。 |
| **R29-P2-03：提前发布 op_id，立即 poll 得 -4** | **已关闭** | `crates/ak-ffi/src/task.rs:1857` 在 request registry 锁未释放前注册 Running entry，然后 `:1859` 才发布 id；`:1903` launcher 接力同一 reservation。新测试源码既在未 launch 的 reservation 上查询 Running，又在并发重试返回后立即 poll，修正上轮“等两个 launcher 都返回后才 poll”的覆盖错位。 |
| **R29-P2-04：角色选择取消选择后对空 Set 取 first** | **已关闭** | `flutter_app/lib/ui/pages/home_page.dart:326` 对空集合置 null，`:257` 的 ready 要求非空角色，`:332` 禁用提交。`flutter_app/test/home_flow_test.dart:263` 起新增再次点击当前选项取消选择的测试源码，覆盖合法空集合分支。 |
| **R29-P2-05：offer gate 捕获完整 TaskPairing 形成 Arc 环** | **已关闭** | `crates/ak-session/src/pairing/task.rs:958` 只克隆 tasks Arc，`:961` 的闭包不再捕获 TaskPairing/orchestrator，原引用环被切断。`:1446` 起的 Weak 测试源码验证安装 gate 后释放 pairing 可回收任务表。此测试仅针对 gate 所有权环，不能据此声称整个 FFI/所有后台任务生命周期均已验证。 |

### 修复间一致性与声明边界

| 交互点 | 本次核对结果 |
|---|---|
| 门禁生产接线与无环修复 | 两项配合成立：FFI 安装的是只持任务表的 gate，先于生产 owner loop；不会把上一轮完整 TaskPairing 强引用环带进生产。无角色请求只是默认方向的特殊情形，不能把默认 Receiver 恰好与创建方同向的测试概括为“缺 role 一律拒绝”。 |
| Commit 错误双侧归因 | 新版 Pairing 域帧已贯通；Code 域分支错误不应被新版服务端测试掩盖。`crates/ak-session/tests/task_pairing.rs:513` 使用新版服务端，仅断言一成一败、0x2109 和小于 8 秒，没有强制两者先同时 pending，也没有独立断言 owner 败者 flow 终态；其注释仍称 Code 域。服务端 `engine.rs:3293` 的裸 TLS 测试强制两 pending 并断言新 Pairing 帧，但绕过生产 orchestrator。两类测试源码合起来仍不是“实际运行过强制 Commit 竞争的生产双端闭环”证据。 |
| disconnected / done 终态 | `local/peer/receiver_failed/done` 在 native、Dart allowlist 和 ffi-v2 中一致；done 既收敛服务行，也从 GUI 活动列表排除；删除 watcher 关闭了原协调通知被错误映射成 receiver_failed 的生产路径。 |
| consent_expired / request_closed | consent_expired 已完成名称与路由对齐。request_closed 在 native 和契约中存在，Dart 仍当 unknown 忽略；正常断开借 disconnected 可以收敛，逆序事件则不能，详见 **R30-P2-03**。`ffi-v2.md:440` 的“UI 本地计时仅兜底”也与已删除本地 Timer 的实现不符，应同步修订。 |
| saveDir 的三个时点 | **TLS accept**：`task.rs:2750` 读取 A，`:2758` 写入 ReceiveSpec；**Offer/登记同意请求**：`:823` 再读当前设置并发 saveDir；**用户同意**：`:2409` 只移除请求并发送 bool，不重新选目录。若在前两个时点之间改为 B，横幅虽使用新字段，字段本身却不是实际目标。若在 Offer 后改为 B，字段保持 A 可使横幅与实际目标一致，但这采用的是不可变目标语义，不能称“同意时解析”。原报告允许同意时解析，或绑定并展示不可变实际目标；当前实现缺少把同一个真实目标贯穿 ReceiveSpec 与事件的保证。Fake 手工注入 saveDir 的展示测试无法证明这一点。 |
| unreachable 与可信路径 | native/list/Dart 状态现已对齐；probe 成败仅供可达性呈现和候选选择，不提升为真实对端身份或路径授权证明。真实文件通道仍须执行既有 QUIC 指纹与路径检查。 |

## 新发现（分级）

本轮确认 **5 项新发现：P0 × 0、P1 × 1、P2 × 3、P3 × 1**。均为源码推导，未动态复现。保存目录二次读取、空闲关闭缺方向、TTL/撤码确认和 Code 归因残项已计入对应旧项，不再次计数。

### R30-P1-01：接收等待在 service.dispose 后继续轮询已释放的原生 engine

- **位置**：`flutter_app/lib/services/ffi_simple_transfer_service.dart:206`、`:311`、`:318`、`:342`、`:347`；`flutter_app/lib/services/simple_transfer_service.dart:360`、`:629`；`flutter_app/packages/ak_ffi_dart/lib/src/task_ffi.dart:459`、`:647`、`:984`；`crates/ak-ffi/src/shim.rs:213`、`:383`。
- **触发**：使用真实 FfiSimpleTransferService，已接受一个尚未完成的接收请求；宿主调用该服务的公开 dispose，Dart isolate 继续运行，接收等待仍停在 50ms 的 delayed 上。
- **证据**：dispose 先调用借用者 `_engine.dispose()`，再调用 engineOwner；生产 `flutter_app/lib/main.dart:105` 将该 hook 接到 owner.dispose。底层 `AkEngineDart.dispose` 在 `engine.dart:123` 调 engineFree；Rust `shim.rs:221` 用 Box::from_raw 释放对象。新 `_awaitReceiveSettled` 的两个 listen 没有 onDone，流关闭不会令 settled 为 true；它在 await 返回后也不检查 disposed，继续调用 drainProgress。借用者 dispose 保留 `_runtime`，drainProgress 不检查 `_disposed`，最终由 task bindings 保存的旧裸 Pointer 调 `ak_poll_progress`；Rust `shim.rs:386` 再对已释放对象执行 unsafe as_ref。
- **影响**：明确违反 FFI 句柄生命周期，形成释放后访问，可导致崩溃或未定义行为。该问题独立于任务是否已在 Rust teardown 中被 abort：悬空的是 engine 指针本身。不能把 catch Dart 异常视为原生内存安全保护。
- **范围限定**：这是“接收进行中调用公开 service.dispose”的可达生命周期缺陷。当前 AppShell 没有把每次页面关闭都接到 service.dispose，因此不宣称“关闭任意页面必然触发”，也不宣称已经观察到真实崩溃。
- **修复方向**：为接收等待增加明确的生命周期终结，在每次 await 返回后、任何 FFI 调用前校验有效性；流关闭必须终结等待。释放 owner 前使所有借用调用通道失效，drainProgress 也应与其他入口采用同样的存活门禁。
- **测试源码缺口**：`flutter_app/test/simple_transfer_service_test.dart:767` 验的是 createCode 等 op waiter 的 ensureLive，不覆盖 adapter 新接收循环。应覆盖“同意接收→等待未完成→dispose→后续定时 tick 不再有任何 native 调用”，本次未执行。
- **置信度**：高。

### R30-P2-01：发送续期每轮重置比较基线，丢失事件泵已读取的真实进度

- **位置**：`flutter_app/lib/services/simple_transfer_service.dart:242`、`:243`、`:552`、`:660`、`:889`。
- **触发**：健康传输持续有进度；默认 50ms 事件泵先读取进度，再由默认 200ms 的发送 op waiter 轮询。
- **证据**：`_progressAdvancedFor` 每次调用先把**此刻** progressTick 存为 baseline，再 drainProgress，最后只比较本次 drain 前后是否增长。它没有保存“上一次发送 waiter 观察”的 tick。事件泵已经归入全局 tick 的行，会被直接纳入新 baseline，从而不算进展。合法时序下，每次发送 poll 前的进度都被事件泵读完，onPoll 持续返回 false，截止时间不延长。
- **影响**：即使真实传输持续前进，仍可能在 120 秒预算到期后调用 disconnect，保留了 P2-10 的核心用户影响。不是每条传输必现，取决于进度到达与两个轮询者的时序。
- **修复方向**：为每个发送 waiter 保存跨轮询的上次观察值，或共享最后进展时间；归并进度和判断是否续期不能只看当前函数自己消费的行。
- **测试源码缺口**：`flutter_app/test/simple_transfer_service_test.dart:546`、`:558` 把事件泵周期设为一天，`:559` 把发送 poll 设为 20ms，再在 `:576` 每 25ms 注入进度，避开了生产双消费时序。应保持事件泵活动，控制其先读进度再让发送 waiter 读取已增长 tick；本次未执行。
- **置信度**：高。

### R30-P2-02：接收方作为加入者时没有进度 code_id，续期失效

- **位置**：`flutter_app/lib/services/ffi_simple_transfer_service.dart:278`；`flutter_app/lib/services/simple_transfer_service.dart:405`、`:436`、`:673`；`crates/ak-ffi/src/task.rs:2667`、`:2855`、`:2895`、`:2957`。
- **触发**：发送方生成 Sender 码，另一台设备选择 Receiver 输入该码，接受需要超过默认 180 秒的正常传输。
- **证据**：adapter 只监听 connId 和 `codeIdOfConnection(conn)`。后者查 `_codeIdOfKey`，而此表仅在本机 createCode 返回后填入；join 结果及 connected 事件没有提供可供该表填入的 code_id。native 却明确允许 Receiver 加入，在 `task.rs:2895` 获取真正 code_id 并写入 receiver slot，所有接收 progress 行都以此 **code_id** 为 job。加入侧 waiter 因而只看 connId，真实进度增长发生在另一个 key 上。`simple_transfer_service.dart:671` 的“加入侧不会接收”注释与产品和 native 实现均矛盾。
- **影响**：在该正常发起方向上，接收一直有真实进度也不续期；未先完成或终止的接收会在默认 180 秒后被误取消。本机创建 Receiver 码的方向可能正常，不能据此覆盖任一侧发起。
- **修复方向**：在双方连接事实中提供并保存同一 code_id/进度关联键，或让接收进度直接使用双方都已有的 connId；冻结契约与双侧映射需一起调整。
- **测试源码缺口**：`flutter_app/test/ffi_simple_transfer_service_test.dart:339` 先在受测服务本机创建 Receiver 码，`:345` 从本机映射取得 code_id，再在 `:354` 注入该 key 的进度，仅覆盖拥有该映射的创建侧。应补发送方生码、异引擎接收方加入的相反方向；本次未执行。
- **置信度**：高。

### R30-P2-03：请求终态先到、incoming 后到时，已关闭请求可永久显示待同意

- **位置**：`crates/ak-ffi/src/task.rs:799`、`:833`、`:841`、`:2197`、`:2203`、`:757`；`flutter_app/packages/ak_ffi_dart/lib/src/model/task_events.dart:108`、`:122`；`flutter_app/lib/services/simple_transfer_service.dart:832`、`:924`、`:973`。
- **触发时序**：原生接收 worker 正在同步 register_consent，另一个 FFI 调用线程同时断开该连接。下面是代码允许的线程交错：

  1. register_consent 将 job 放入 pending，释放 pending 锁，尚未发布 incoming_request。
  2. task_disconnect 先发布 disconnected，再由 decline_pending_of_conn 移除该 job、发布 request_closed 并 send(false)。
  3. 接收 worker 完成当前同步 callback，随后发布迟到的 incoming_request。Tokio abort 在后续 yield 才生效，不能撤销已经运行中的同步 callback。
  4. 即使本轮登记的 30 秒 timer 随后运行，它也因 pending 中已无该 job 而不再发 consent_expired。

- **证据**：native 的 pending 插入与 incoming 入队不是同一临界区。Dart 处理先到的 disconnected 时，`_pendingJobs` 还没有这个 job；request_closed 没有对应解析分支，变为 TaskUnknownEvent 后被忽略；迟到 incoming 在 `simple_transfer_service.dart:925` 无条件登记并展示，不检查连接已终态，也没有请求关闭记录。此时本轮又删除了原本会最终清理横幅的本地 Timer。
- **影响**：native 已拒绝并移除的请求在 UI 上长期显示“接受/拒绝”；没有后续自动关闭事实，用户点击接受才发现 -4。这不是声称正常 incoming→disconnected 顺序仍然失效，而是本轮请求事件衔接与去掉 Timer 后的确定时序缺口。
- **修复方向**：把 native 请求登记/发布与关闭串成一致顺序；Dart 应处理 request_closed，并通过连接终态或有界的请求关闭记录拒绝迟到 incoming。仅增加一个 request_closed 分支而不记住先到终态，仍不足以解决该时序。
- **测试源码缺口**：`flutter_app/test/simple_transfer_service_test.dart:721` 起只按 incoming→pump→disconnected 顺序检查关闭；原生 `task.rs:5629` 附近的到期/断开测试也先完成注册再关闭。应控制 pending 已插入、incoming 尚未入队的窗口，同时覆盖终态先到的 Dart 序列；本次未执行。
- **置信度**：高，来自合法的 FFI 线程与 Tokio worker 交错。

### R30-P3-01：新增 handshake_failed 冻结事件形状与原生输出不一致

- **位置**：`docs/contracts/ffi-v2.md:433`；`crates/ak-ffi/src/task.rs:2741`；`flutter_app/packages/ak_ffi_dart/lib/src/model/task_events.dart:122`。
- **证据**：新契约示例含 `conn` 和表示整数含义的 `code:"<int>"`；实际 native 事件没有 conn，`code` 存放的是 32hex 的 **code_id** 字符串，另有固定 reason。未经配对的失败握手本来也可能没有可归属的 conn，契约不能要求一个始终存在的连接 id。
- **影响**：依照新冻结契约实现的日志/SDK 消费者会取得错误字段含义或缺失关联，后续对齐存在兼容风险。当前 Dart 把该事件视为 unknown，未按上述错误 schema 强制解析，故这里没有证据支持“当前 GUI 解码崩溃”，按文档/接口一致性问题列 P3。
- **修复方向**：明确 code_id、可选 conn 和固定 reason 的真实含义，统一文档与输出；不要把任务码 id 冒充整数错误码。
- **置信度**：高。

## 总结论

**不通过。** 原 21 项为 **已关闭 15、部分 6、未关闭 0**；上一轮新增 5 项为 **已关闭 4、部分 1、未关闭 0**。26 个历史跟踪条目合计 **已关闭 19、部分 7、未关闭 0**。本轮新增 **5 项：P0 × 0、P1 × 1、P2 × 3、P3 × 1**。

| 跟踪分组 | 已关闭 | 部分 | 未关闭 | 合计 |
|---|---:|---:|---:|---:|
| 原 P1 | 5 | 1 | 0 | 6 |
| 原 P2 | 10 | 5 | 0 | 15 |
| 原发现合计 | 15 | 6 | 0 | 21 |
| 上轮新增 R29 | 4 | 1 | 0 | 5 |
| 历史跟踪条目总计 | 19 | 7 | 0 | 26 |

本轮实质关闭了上轮两个未关闭生产问题：owner 门禁已经安装，协调面 watcher 已删除；握手失败后的 listener 寿命、op 登记可见性、claim 历史编码、同流多订阅快照、done 终态和 native unreachable 也有明确源码修复。P1-03/R29-P2-01 的“部分”专指本轮声称的 Code 域归因分支残留，本版 Pairing 域修复和单次额度安全结论不被推翻。

不通过的决定性依据是新的接收释放后访问原生指针，以及两条健康长传输仍被误取消的实际续期路径。除此以外，空闲关闭仍缺一个方向；未加入码的 TTL 本地回收/服务器撤码确认尚未闭合；saveDir 事件与真实 ReceiveSpec 可能不一致；请求终态先到的竞态仍可留下永久横幅。上述缺口不足以支持“所有复审发现均已关闭”的声明。

证据边界：提交说明中的 workspace 1118/0、Dart 134/134、Flutter 124/124、clippy/契约通过及服务器/APK 更新，均属于提交者的历史验证声明，本轮没有复跑或独立验证。本报告对新增测试仅评价源码路径与覆盖边界，既不把它们写成实际通过，也不把静态发现写成测试实际失败。README/HANDOFF/ROADMAP 仍有 wave-29“进行中/未提交”的滞后文字，不能据其判断本版验收完成。

本次修复差异中未确认新增 TURN/relay、HTTP 文件上传、云端暂存或第三设备代传路径，也未确认新的文件路径越界或符号链接绕过；文件接收沿用既有身份、路径和存储防护。这只是本次差异的只读结论，不是抓包证明、所有部署网络无代理证明或平台存储防护的重新验收。**本轮构建执行数为 0，测试执行数为 0，仓库/语法检查执行数为 0；只新增本报告。**
