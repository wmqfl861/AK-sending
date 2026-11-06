# AK-sending 第二十九波修复复核报告

评审日期：2026-11-07。评审对象：`77d95597cf0731f8f48f0a492f092942aaf3e7b8`，修复差异基线为 `7458e9f`。原发现依据：[第二十八波终审报告](codex-review-2026-11-06-wave28.md)。

本次按注册服务器、会话库、Flutter/Dart 三个模块并行只读审阅，由集成评审补查 FFI、引擎及跨模块调用链。已阅读 README、HANDOFF、ROADMAP、ADR-001、相关契约、实现及测试源码。开始时工作树干净；唯一新增文件是本报告。**没有执行任何构建、测试、仓库检查、语法检查、设备实验、网络实验或部署操作。** 下文的“已关闭”仅表示源码能够关闭相应触发路径，不代表动态验证、真机验收或现有二进制已验证。

判定口径：同时核对原发现的触发／影响与本次完整修复声明。原主要用户场景已修复，但声明中的契约或其他明确分支仍有缺口时，列“部分关闭”，并明确剩余范围；不会把文档残留、API 边界等残项自动按原 P1 严重度重新定级。新问题以 `R29-P2-*` 编号，与原发现交叉引用；不把同一残留影响重复计为另一项新发现。

## 逐项复核表

### 原 P1 发现（6 项）

| 原发现与原触发 | 本次实际修复 | 判定 | 当前证据及未闭合部分 |
|---|---|---|---|
| **P1-01：实际 main 无条件选 Fake，真实 GUI 无法配对传输** | main 构造 facade，借用 engine，检查任务导出，创建真实 bindings 与服务；Android 等待 path_provider 身份目录；启动错误进入可见错误卡；释放时先停借用者再释放 owner。 | **已关闭** | `flutter_app/lib/main.dart:41`、`:85`、`:89`、`:100` 的生产链为 `AkEngineDartFacade → borrowEngine → AkTaskFfiBindings → FfiSimpleTransferService`；`flutter_app/lib/services/ffi_simple_transfer_service.dart:119`、`:177`、`:384` 解析并等待 Android 身份目录，`:202` 按顺序释放；`flutter_app/lib/ui/app.dart:103`、`:137` 捕获并显示 start 失败。Fake 不再是 main 的生产实现。没有运行 GUI、DLL 或 Android，不能据此声称跨设备流程已验收。 |
| **P1-02：UI 八位码规则拒绝全部真实 AKI1 码** | UI 统一使用 `AKI1` 前缀加 32 个 Crockford 符号，规范化后共 36 字符；显示为前缀加八组四字符，输入、展示、复制、控制器和服务传参已对齐。 | **部分关闭** | **原功能阻断已关闭。** `flutter_app/lib/ui/service/simple_transfer_service.dart:201`、`:209`、`:216`、`:233`、`:240` 定义统一规则；`flutter_app/lib/ui/pages/home_page.dart:254`、`:280` 支持完整码，`:409` 复制、`:446` 展示；`pairing_flow_controller.dart:100` 校验；Rust `crates/ak-identity/src/invite.rs:164` 为真实生成格式。但“AKT1 清理”未完成：`docs/contracts/ffi-v2.md:397`、`:409`、`:425`、`:438` 仍在 join/create/event/list 示例中使用 AKT1。剩余是冻结契约不一致，不能误述为当前真实码仍不可输入。 |
| **P1-03：多个 pending 越过单次额度，均可 Commit** | Commit 的同一个 Immediate 写事务重新读 `max_redeems` 并统计已提交数量；满额时将当前败者转 aborted；新增双端额度错误发送。 | **部分关闭** | **原越额不变量已关闭。** `crates/ak-signal/src/server_v2/store.rs:974` 开事务，`:1016` 重读，`:1042` 只数 committed/applied/ready，`:1048` 满额 abort，`:1077` 才允许 Commit。pending/aborted 不占额度。但 `engine.rs:1853` 新发的 `request_id=0, scope=Code, object_id=code_id, 0x2109` 被真实 `RegistryClient` 分派丢弃，orchestrator 也不接这种错误；双端业务仍等到超时。详见 **R29-P2-01**，不能将“TLS 两端收到了帧”当作“真实双方状态机收到了明确失败”。 |
| **P1-04：任意自签身份冒用合法请求归属，并可消耗唯一接收器** | owner 完成回调同步登记已验证指纹；receiver authorize 和 consent 归属均核对该码的对端指纹；已认证但未授权的拨号会回到 accept 循环。 | **部分关闭** | **身份冒用分支已关闭。** `crates/ak-ffi/src/task.rs:1180`、`:1195`、`:1201` 同步登记，`:1316` 保存已验证指纹；`:2588` 只允许该指纹；`:706`、`:761` 按实际 TLS 指纹归属 Offer；`crates/ak-engine/src/lib.rs:3550` 从真实 channel 取 `sender_fingerprint`，拒绝在 Accept 和用户文件写入之前。但 `task.rs:2594` 仅对 `Unauthorized` 继续，其他握手错误直接 return。`crates/ak-net/src/lib.rs:528`、`:531`、`:591` 显示单次对端握手中断／证书失败返回 Transport／PeerIdentity，并不表示 listener 已坏；陌生设备触发这种失败仍会关闭监听，合法任务之后不能接收。详见下文接收循环残留。 |
| **P1-05：同向或缺 endpoint 的兑换在 Commit／落簿后才失败，消耗唯一额度** | session 增加 offer gate，Dart/UI 传显式本机角色；库测试覆盖拒绝后的码复用及无 journal／簿。 | **未关闭** | **生产 FFI 没安装新门禁。** `crates/ak-session/src/pairing/task.rs:991` 才调用 `set_redeem_offer_gate`；生产 `crates/ak-ffi/src/task.rs:1209` 直接走 `pairing.orchestrator().spawn_owner_loop_verified(...)`。gate 初始为 None（`orchestrator.rs:301`），因此真实 owner 仍放行错误任务上下文。双方完成配对后才由 FFI `:1195` → session `task.rs:849`、`:864`、`:870` 检查缺 blob／方向冲突，唯一额度及信任关系已产生。显式选角色改善正常加入，但选错角色、无角色 FFI 默认 Receiver 或旧客户端缺 blob 仍触发原副作用。额度复查不能替代此门禁。 |
| **P1-06：后续连接流订阅者无快照，详情页误判断开** | connections getter 每次创建一个新 wrapper，先订 live 再发送当前快照；主页与详情分别获取 getter。 | **部分关闭** | **原主页 → 详情场景已关闭。** `flutter_app/lib/services/simple_transfer_service.dart:433`、`:437`、`:442` 为每次 getter 提供快照，详情 `connection_detail_page.dart:42` 确实重新读 getter。但 wrapper 自身仍是 broadcast，快照仍放在只对 0→1 监听数调用的 onListen 中。保存 `final s = service.connections` 后先后对同一 s 调用两次 listen，第二订阅仍无初始快照。因此是“每次获取流”修复，尚非声明的“每个订阅者”；新增测试每次重新读 getter，没有覆盖同一流的第二订阅。此残项不等于原详情页仍然损坏。 |

### 原 P2 发现（15 项）

| 原发现与原触发 | 本次实际修复 | 判定 | 当前证据及未闭合部分 |
|---|---|---|---|
| **P2-01：request_id 非原子去重，未绑定 API／conn，终态后不能重放** | 单锁查重和预留 op_id；canonical 包含操作域和 conn_id；重放先于连接活性检查。 | **部分关闭** | `crates/ak-ffi/src/task.rs:1675`、`:1714`、`:1735`、`:1747` 关闭重复启动、跨 API／conn 错配；`:1973` 经同一 gate，旧 conn 记录保留使断开后重放可用。但只把预留 op_id 写入 request_ids，尚未同步登记 ops；并发重试可在 `launch_task_op:1802` 之前返回一个暂不可查询的 op_id，第一次 poll 得 -4 并失败。详见 **R29-P2-03**。 |
| **P2-02：并发首次初始化重复登录，保留已被踢下线的旧栈** | 增加异步初始化互斥门，锁内二次查 stack，跨 TLS 登录和安装全程持锁；teardown 竞争时关闭未安装客户端。 | **已关闭** | `crates/ak-ffi/src/task.rs:513` 定义 async Mutex；`:1116` 获取后再次查槽，直到 `:1250` 才释放；`:1128` 的登录仅由该临界区执行；`:1225` 再查 stopped，`:1240` 对 teardown 后的未安装栈 abort 并 close。不同 request_id 也共享此门，不只是相同请求去重。源码可排除原“双初始化登录互踢、保存旧 client”时序；未运行并发测试。 |
| **P2-03：已配对但空闲时单侧断开不通知远端** | 新增钉扎 QUIC TASK-CLOSE 控制帧尝试通知远端，保留同 engine 快速通知。 | **部分关闭** | `crates/ak-ffi/src/task.rs:2217`、`:2236`、`:2246` 实现通知，但只有 receiver 绑定数据 listener；sender 在 `:2370`／`:2709` 广告端口 0 和 UDP echo。接收方主动断开时无法用 QUIC 通知空闲发送方，原问题依旧；代码 `:2049`～`:2053` 自己承认这一方向只能等下次发送。发送通知没有业务确认，revoke 错误在 `:2126` 被丢弃。新测试 `:4673` 仅覆盖 sender 断开通知 receiver。 |
| **P2-04：未加入码不能取消，默认无限 TTL，资源／注册行积存** | native 支持以 code_id 取消；0／省略 TTL 改为 600 秒；teardown 有界尝试撤码。 | **部分关闭** | `crates/ak-ffi/src/task.rs:1863`、`:2067`、`:2138`、`:577` 具备实质修复。但 GUI 取消 `flutter_app/lib/ui/pages/home_page.dart:488` 仍只 reset，`pairing_flow_controller.dart:147` 只清界面字段；`services/simple_transfer_service.dart:351`～`:363` 丢弃 create 返回的 code_id，只把 code 字符串交上层，无法调用新增撤码入口。用户取消后旧码仍能兑换直到过期。session `task.rs:792` 的 active task 记录不保存到期时间，FFI `task.rs:2411`～`:2435` 也未安排 TTL 清理 receiver／echo／缓存；注册行过期不等于本地资源回收。native 撤码结果仍被 `:2191` 忽略。 |
| **P2-05：接收器只接一次，成功／拒绝后消失而 UI 仍 connected** | receiver 改循环，ConsentDeclined 后 continue；成功定义为单任务完成，双端 native 发 disconnected/done。 | **部分关闭** | `crates/ak-ffi/src/task.rs:2572`、`:2633` 使拒绝后可重试；`:2629`、`:2878` 在成功后结束 native conn。但 Dart `simple_transfer_service.dart:675` 的终态原因表没有 `done`，`:789` 将它当协调通知丢弃；`:778` 的 TaskDone 也只转发、不结束行。因此成功后 UI 仍给出已失效的发送入口，下一调用被 native -2 拒绝。此为本项残留及 P2-08 修复交互，**不重复计为新发现**。 |
| **P2-06：生码时捕获保存目录，后来改设置仍落旧目录** | 将目录快照延后到每个已接受 TLS 连接开始处理时。 | **部分关闭** | `crates/ak-ffi/src/task.rs:2601`～`:2613` 关闭“生码后、发送前改目录”的场景，却仍在 Offer／用户 consent 前冻结目录。pending consent 时把 A 改为 B，接受后仍写 A；UI `incoming_requests_host.dart:117` 总取当前 `service.saveDir`，`:212`～`:213` 显示“保存在 B”。没有每次请求的真实目标字段，也未在同意时解析目标，原报告明确列出的 pending 场景仍在。 |
| **P2-07：同步 Condvar 等待占满两个 Tokio worker** | consent 返回 Now 或 oneshot Receiver；引擎 await pending；定时拒绝使用异步 timer；断开移除并拒绝 pending。 | **已关闭** | `crates/ak-engine/src/lib.rs:692` 的 `ConsentAnswer` 和 `:3552` 的 await 代替 worker 内阻塞；FFI `task.rs:752` 注册并立即返回，`:796` 异步计时，`:725`、`:2083` 断开唤醒，`:542` teardown 丢弃 sender 后 fail closed。新测试源码 `task.rs:4925` 明确构造两个 pending consent 加第三个控制操作。没有运行该测试；P1-04 的握手错误退出、P2-11 的过期通知缺口并不意味着 Condvar 饥饿仍存在。 |
| **P2-08：协调连接断开误杀仍健康的数据任务状态** | Dart 增加断开原因 allowlist，试图区分协调通知。 | **未关闭** | 真实 session watcher `crates/ak-session/src/pairing/task.rs:1026`、`:1033`、`:1042` 仍对 registry 断开将所有 task 结束；FFI `task.rs:1271`～`:1275` 把 Network 映射为 `receiver_failed` 并置原生 conn disconnected；Dart `simple_transfer_service.dart:675`～`:679` 正把该值认作终态，`:794` 继续判死。后续发送又被 native `task.rs:1767` 拒绝。新增 Fake 测试注入 `registry`，不是生产会发出的原因。没有证据支持该项已经完成分离。 |
| **P2-09：忽略失败探测，迟到 join 结果回退／复活状态** | Dart 单调归并；probe true／false／null 分别处理；UI 加 unreachable 状态。 | **部分关闭** | `flutter_app/lib/services/simple_transfer_service.dart:399`、`:618`、`:745` 修复 UI 时序及失败提示，adapter `ffi_simple_transfer_service.dart:403` 映射 unreachable。但 Rust `crates/ak-ffi/src/task.rs:1340`～`:1344`、`:2791`～`:2796` 仍在任何 probe outcome 后无条件将 Probing 改为 Connected；`ak_task_list` 和 native 受理状态仍与 UI 不一致。没有发现这直接绕过真实 QUIC 身份或路径授权；问题仍是状态真实性。 |
| **P2-10：120／180 秒整体超时只停 UI，native 继续工作** | 超时后 best-effort disconnect；接收等待也监听连接终态。 | **部分关闭** | `flutter_app/lib/services/simple_transfer_service.dart:496` 和 adapter `ffi_simple_transfer_service.dart:281` 增加取消，关闭了正常取消成功时的后台继续传输。但默认 **120／180 秒整体时限仍在**（adapter `:102`、`:103`；`task_ffi.dart:751`、`:762`），没有按进度续期。持续推进的大文件会被自动切断，原合理长传输仍误判失败。取消错误在 engine service `:512` 和 adapter `:300` 被吞，随后文案无条件说“已取消”，也未等原生终态确认。详见下文长传输残留。 |
| **P2-11：原生自动拒绝后横幅不关，点击拒绝抛未处理异常** | 过期拒绝幂等；UI 接受和拒绝均捕获异常；新增 expired stream 和本地 Timer。 | **部分关闭** | adapter `ffi_simple_transfer_service.dart:250`～`:258` 与 `incoming_requests_host.dart:77` 关闭拒绝未处理异常；`:54` 订阅过期流移除横幅。但实际到期源仍是 Dart 在收到 incoming 后另起 30 秒 Timer（`simple_transfer_service.dart:769`），native `task.rs:796`～`:800` 到期只 remove/send(false)，没有请求终态事件。事件积压／后台恢复可在 native 已拒绝后重新显示“可接受”横幅再等 30 秒；连接提前断开也不驱动该横幅立即关闭。不能把本地计时镜像当作后端期限同步。 |
| **P2-12：UDP echo 响应仍被应答，形成持续互回** | 全零 nonce 作为响应标记，接收该标记时不回应；添加每来源限频和有界来源表。 | **已关闭** | `crates/ak-ffi/src/task.rs:1472` 先验证包形状，`:1478` 对全零 nonce 直接 continue，`:1505` 所有回复均带该标记，故两个新版 responder 不会继续互回。原持续循环的必要反馈已切断。边界：当前不是会话认证探测，仍允许 1:1 反射；`:1495` 满表 clear 会重置来源预算，也无全局回复预算。不能将此次关闭夸大为已验证全面抗洪泛或已认证真实路径。 |
| **P2-13：v1→v2 迁移非原子，中断留下无法启动的半迁移库** | DDL 与 user_version 同写事务；检查既有列后仅补缺列，兼容旧半迁移；新建库也原子。 | **已关闭** | `crates/ak-signal/src/server_v2/store.rs:1336` 开 Immediate 事务，`:1339` 查询列，`:1345` 补缺列，`:1348` 写版本，`:1349` 统一提交；`:403` 初建同理。未发现删除旧表、tombstone 或唯一性记录的恢复分支。新增测试源码 `:2966` 构造 1／3／4 列已落地的旧半迁移；本次未运行，也没有进行断电／故障注入。 |
| **P2-14：claim canonical 编码变化破坏旧 journal 幂等重放** | 默认扩展恢复 v1 的 36B canonical body，仅非默认扩展追加尾部。 | **部分关闭** | `crates/ak-signal/src/server_v2/store.rs:1516`、`:1525` 关闭 v1 的 36B 旧记录重放问题，但 wave-28 已持久化的默认记录是 41B；schema=2 没有转换，`:478` 仍逐字节比较。升级 wave-28→wave-29 时原请求反而冲突，详见 **R29-P2-02**。不能仅验证“v1 直接升级本版”就承诺所有受支持来源版本兼容。 |
| **P2-15：endpoint cache 历史失败无限积存** | 增加 256 项 FIFO 硬上限；Pairing 范围错误、消费、断线清理表和队列。 | **已关闭** | `crates/ak-session/src/pairing/client.rs:166` 设上限，`:220`～`:230` 超限淘汰，`:238`～`:249`、`:343`、`:551` 回收。原“失败历史越多、缓存无界增长”的影响已消除。边界：本机 abort／无消费者并非立即清理，FIFO 可淘汰活动提示，不能称完整生命周期回收；残留仍受 256 项约束。正常 owner 在 PairReady 后才消费 blob，不能为追求终态清理而直接在 PairReady 到达时删除。没有确认足够具体的生产活动项淘汰故障，本项不据此另增确定发现。 |

## 尚未闭合的关键触发路径与修复交互

### 1. P1-05 门禁与 P1-03 额度复查：库内正确，GUI 实际绕过

门禁本身在 `crates/ak-session/src/pairing/task.rs:946` 检查 blob 存在、能解码且方向互补。`orchestrator.rs:787` 读 gate，拒绝后 `:813` 发 PairAbort 并 continue，发生在 `owner_answer_offer` 和 journal／落簿之前。服务器只将 pending/prepared 转 aborted，计数又只纳入 committed/applied/ready；**若此门禁真正被调用**，两项修复在数据层能够配合。

实际 FFI 为了同步登记指纹而自行创建 on_paired 回调，却直接调用下层 `spawn_owner_loop_verified`，绕过安装 gate 的包装器。可达触发是：A 的真实 GUI 创建接收码；B 选择相同方向加入，或旧调用方带码不带 endpoint；owner 没有 gate，先完成可信配对，随后任务完成回调才报方向／endpoint 错误。此时“max_redeems=1”仍被这笔错误任务消耗，B 改为正确方向也不能重新正常兑换。P1-03 能保证最多一次 Commit，却不能保证这一次是任务上下文正确的 Commit。

测试源码对这一断层有直接解释：`crates/ak-session/tests/task_pairing.rs:287`、`:357` 的负例调用了有 gate 的 `TaskPairing::spawn_owner_loop`；`:400` 另有 gateless owner 测试，展示的正是当前 FFI 仍走的提交后失败形态。不能用前者覆盖后者。修复应提供可保留 FFI 同步完成回调、同时强制安装前置 gate 的统一入口；仅调整测试或 UI 选角色不够。

### 2. P1-04 接受循环与 P2-07 异步同意：身份检查有效，监听寿命仍被单次握手控制

合法证书但指纹不在该任务中的陌生设备，会在 `task.rs:2596` 被拒并继续监听；ConsentInfo 也确实来自 `DirectChannel::peer_fingerprint`，没有把 Observation 枚举／布尔值当身份凭证。未知归属 Offer 不产生用户提示，不进入 Accept、用户文件、分片或检查点记录写入。这里“零落盘”不包括预先创建保存目录、身份目录或检查点目录，不能宣称绝无文件系统操作。

剩余触发无需配对或任务码：只要陌生设备可达监听端口，制造一次进入 QUIC accept 后的握手中断／不合法证书失败，`DirectEndpoint::accept` 就可能返回 Transport／PeerIdentity；FFI 的 `Err(_) => return` 将单次连接错误当成整个 endpoint 结束。接收器退出时又未给已有 task 发明确终态，形成端口消失、UI 仍认为配对可用。有效自签陌生者被拒的正例不能证明所有陌生拨号都不会耗尽 listener。

`task.rs:4449` 起的新身份测试专门选用“知道 receiver pin，能够完成 TLS”的陌生者，恰好只覆盖 Unauthorized 分支；需要保留这条正确信任边界，同时把可恢复的单连接握手失败和真正 endpoint 关闭分开处理。异步 consent 已解除 worker 饥饿，但不会自动修复这个接受循环退出条件。

### 3. P2-05 终态与 P2-08 原因过滤互相抵消；协调断线仍触发旧缺陷

成功传输后 native 双端都发 `done`，随后发 `disconnected(reason="done")` 并拒绝再次发送，这是本波采用的单次任务结束语义。Dart 新原因过滤却只允许 local／peer／receiver_failed，导致成功终态被丢弃，完成连接仍留在列表中。应冻结并统一完整的终态原因语义，不能由 Dart 将所有未知原因自动认作无害协调通知。该现象已在 P2-05 计为部分关闭，不另计独立新问题。

另一方面，真正的协调断线仍由 session watcher 产生 Network，再被 FFI 改名 receiver_failed，而这个值恰好在 Dart 允许终态列表里。因此 P2-08 连原用户可见影响都没关闭；已经运行的文件流可能继续而状态被判死，后续 send 直接被拒，不能把它描述为“数据流一定当场中断”。`flutter_app/test/simple_transfer_service_test.dart:364` 手工注入 reason=registry，只证明一个生产未产生的字符串会被过滤。

### 4. P2-04 取消、P2-06 保存目录、P2-11 过期仍缺少请求级事实

取消按钮只清 UI，不持有原生 code_id；600 秒 TTL 关闭长期有效注册行，却不替代用户明确撤销，也不回收本地 listener／echo。native 取消入口自己的撤码是异步 best-effort，错误没有作为待同步结果暴露。不能把“返回 OK／已取消事件”扩大成“服务器一定撤销完成”。

保存目录只从“生码时”移到“TLS accept 后”快照：在用户尚未同意时改变设置，文件和 UI 展示仍可分离。`task.rs:4871` 的新测试是在发 Offer 前改目录，因此不能覆盖 pending consent 内改目录；应让请求绑定并展示真实目录，或在同意时解析并固定该次接收目录。

过期也是同一类错位：native 从登记请求开始计时，Dart 从读到 incoming 事件才计时。两者之间存在队列、UI 停顿和后台恢复延迟。新的异常收容正确，但没有原生请求关闭事件、截止时间或当前状态查询，镜像倒计时仍无法证明横幅有效。修复应贯通明确请求终态，并在断开、拒绝、超时等路径关闭对应请求，而非继续让 UI 猜期限。

### 5. P2-10 对长传输的修复把“UI 假失败”变为“健康传输被切断”

发送依然在整个操作开始后 120 秒截止，接收同意后依然 180 秒截止；源码没有按持续进度延长预算。任何需要更长时间的正常扫描／传输都会触发断开，与原报告建议的“终态跟踪＋无进展时限”不同。正常 disconnect 成功时确实降低了 UI 已报错而后台继续落盘的风险，但没有关闭长任务误判。

`_cancelTaskConnection`／`_cancelReceiveTask` 捕获并丢弃取消异常，之后仍无条件声称“已取消”，也没有等待接收 future 或 native op 的停止确认。新增 `simple_transfer_service_test.dart:410` 用 Duration.zero 和不完成的 Fake 验证曾调用 disconnect，无法证明长传输不中断、取消失败可见或终态一致。后续应保留 op／request 跟踪，并将“请求取消”和“取消完成”分开，不再用固定总时长限制持续正常工作。

## 修复引入的新问题

确认 **P0 × 0、P1 × 0、P2 × 5**。其中三项同时影响对应旧发现的完整关闭判定，已在逐项表交叉引用；它们是新增根因，不表示有 21＋5 个彼此无关的问题。原身份接收循环残留、完成后假在线等不在此重复计数。以下置信度均针对源码路径，均未动态复现。

### R29-P2-01：Commit 满额新错误帧无法进入真实客户端业务流

- **位置**：`crates/ak-signal/src/server_v2/engine.rs:1853`、`:1860`；`crates/ak-session/src/pairing/client.rs:268`、`:274`、`:277`；`crates/ak-session/src/pairing/orchestrator.rs:1568`。
- **触发**：同一码的两个 pending 竞争 Commit，第一笔获得唯一额度，第二笔在本波新增事务复查中被 abort。
- **证据**：服务器向 owner 和败者发送 request_id 全零、scope=Code、object_id=code_id 的 0x2109。生产 RegistryClient 只匹配同 request_id waiter，或对 Pairing scope 向配对 inbox／plane 转发；当前配对不存在全零 request waiter，Code 错误因此被丢弃。即便单独补 plane 广播，orchestrator 的 wait_frame 也只认 Pairing scope，缺少与具体败者 pairing_id 的关联。
- **影响**：数据库额度正确，败者没有第二次 Commit；但双方业务不能及时收敛为明确“额度已用完”，继续等 PairCommit 并最终 Timeout，破坏本次声明的“双端 0x2109”。同码 winner 与 loser 的 owner flow 也不能靠无差别广播 Code 错误安全区分。
- **修复方向**：先在契约明确错误的具体配对关联，再同步修改发送、分派、等待状态机。`docs/contracts/protocol-v2.md:266`、`pairing-flow.md:84` 本波明确冻结 Code scope，不能仅在一端悄悄换 scope。
- **覆盖缺口**：`engine.rs:3285` 的新测试使用原始 TLS TestClient，`:3364` 断言帧内容，绕过生产 RegistryClient 和 orchestrator。需要通过真实客户端验证败者双端终态且不误伤赢家；本次未执行。
- **置信度**：高。

### R29-P2-02：恢复 v1 claim 编码时破坏 wave-28→wave-29 持久幂等

- **位置**：`crates/ak-signal/src/server_v2/store.rs:409`、`:473`、`:478`、`:1516`、`:1525`；对照 `7458e9f` 同文件的旧 claim_body。
- **触发**：wave-28 已提交一个默认 claim（max_redeems=0、endpoint=None）并保存 journal；响应丢失或客户端重试，升级到本版后使用相同数据库和相同 request_id／参数。
- **证据**：wave-28 无条件编码为 digest[32]＋ttl[4]＋零 cap[4]＋零 presence[1]，共 41B；本波恢复为 36B。schema 已是 2，init 不迁移 journal；claim 仍要求 stored_body 与新 body 完全相等，因此同一请求固定落入 RequestConflict。
- **影响**：v1 直接升级的旧缺陷关闭，却反向破坏已经部署过的 wave-28 记录；正常新 claim 和旧客户端新请求成功均不能覆盖该恢复场景。
- **修复方向**：在确认是 claim 且扩展为默认时兼容 36B 和尾 5B 全零的历史 41B，或对历史 journal 做受控原子迁移；真实 TTL／cap／endpoint 差异仍必须冲突，不得泛化为只比较前缀。
- **覆盖缺口**：`store.rs:3030` 仅构造 v1 的 36B fixture；`:2832` 只验证当前函数默认长度。应增加来自 wave-28 实际编码形状的 schema-v2 记录，覆盖重放及真实参数冲突；本次未执行。
- **置信度**：高。

### R29-P2-03：幂等 gate 提前发布 op_id，重试可能得到尚不存在的操作

- **位置**：`crates/ak-ffi/src/task.rs:1735`、`:1747`、`:1870`、`:1781`、`:1802`、`:3097`；`flutter_app/packages/ak_ffi_dart/lib/src/task_ffi.dart:723`、`:734`。
- **触发时序**：线程 A 在 gate 中把 `(request_id → op_id)` 登记后释放锁，尚未进入 launch 的 ops.insert；线程 B 使用相同 request_id 重试，直接命中 Replay 并返回该 op_id；B 立即调用 op_result，A 此时尚未登记对应 OpEntry。
- **证据**：request_ids 与 ops 是两张独立锁表；Fresh 分支返回后才调用 launcher，没有等待或同一原子登记。op_result 对 ops 中缺失项固定返回 -4，不视为 Running。Dart poll 遇到 -4 立即抛 AkFfiException，不重试暂缺操作。
- **影响**：重复副作用确实被消除，但“成功受理／重放即返回可查询操作”的契约出现新窗口；调用方已报告失败，原操作随后仍正常创建／兑换／发送。此窗口需要并发原生调用，不应宣称当前单 isolate GUI 每次都会触发。
- **修复方向**：在对其他重试者发布 op_id 之前完成 Running OpEntry 登记，或让请求表保存可直接查询的共享 entry；原子受理应覆盖答案的可见性，再启动副作用。
- **覆盖缺口**：`task.rs:4525` 的新并发测试先等待两个线程都返回，再由一个线程 poll，此时两个 launcher 已完成登记，因此无法发现此窗口。应在重试线程返回后立即查询，并控制首线程停在预留和登记之间；本次未执行。
- **置信度**：高，来自合法线程交错，不依赖锁中毒等异常条件。

### R29-P2-04：新增角色选择器允许空集合，却无条件取 first

- **位置**：`flutter_app/lib/ui/pages/home_page.dart:318`～`:321`。
- **触发**：进入输入码面板，点选“发送文件”或“接收文件”，再点一次同一选项取消选择。
- **证据**：SegmentedButton 明确设置 `emptySelectionAllowed: true`，回调可合法收到空 Set；`setState(() => _role = selection.first)` 无条件取首项，抛 `StateError: No element`。
- **影响**：普通交互产生未处理的框架回调异常，方向状态不能按控件允许的语义清空。不能据此扩大为原生进程必然崩溃。
- **修复方向**：使用可空首项并令 role=null，或一致地禁止取消选择，确保初始状态和按钮启用规则同时成立。
- **覆盖缺口**：新增 UI 测试覆盖首次选择和加入，没有二次点击当前选项的覆盖；本次未执行。
- **置信度**：高。

### R29-P2-05：offer gate 捕获完整 TaskPairing，形成强引用环

- **位置**：`crates/ak-session/src/pairing/task.rs:717`、`:991`～`:995`；`crates/ak-session/src/pairing/orchestrator.rs:250`、`:273`、`:319`。
- **触发**：使用公开的 `TaskPairing::spawn_owner_loop()`，之后 abort owner loop、close client，并释放全部外部 TaskPairing／orchestrator 引用。
- **证据**：新 gate 闭包捕获 `pairing.clone()`，闭包被保存进同一个 orchestrator 的 `Arc<Mutex<Option<RedeemOfferGate>>>`。所有权环为 `offer_gate → closure → TaskPairing → orchestrator → offer_gate`，没有 Weak 或注销路径，外部 handle 消失不会使引用计数归零。
- **影响**：配对上下文、任务表、client 缓存及内存中的 key／邀请码秘密不能随拥有者释放，反复创建销毁该库路径会累计保留资源。**当前生产 FFI 恰好遗漏了此 gate，因此不声称当前 GUI 已触发该泄漏；该问题影响现有公开库路径，并会随按当前方式补齐门禁接线进入 GUI。** 没有证据表明秘密因此被发送或写入仓库。
- **修复方向**：gate 只捕获校验所需的任务表 Arc，或采用 Weak／显式注销，避免捕获包含 gate 容器的完整对象。
- **覆盖缺口**：现有门禁测试验证拒绝语义，没有证明关闭后上下文可回收；本次未执行生命周期验证。
- **置信度**：高，来自明确 Arc 所有权环。

## 文档、测试源码与实际证据的边界

本版 README、HANDOFF、ROADMAP 和 CHANGELOG 仍将 wave-29 写作“进行中／未提交工作树”；HEAD 实际已是提交 `77d9559`。这类状态滞后不另计独立缺陷，但不能把其中“已完成”文字当作本版验收结论。尤其 `docs/HANDOFF.zh-CN.md:63` 称“终态跟踪替代固定超时”，与代码保留 120／180 秒固定 deadline 直接不符；AKT1 契约示例残留已计入 P1-02。

仓库中历史 Rust 1085/0、Dart 129/129、Flutter 107/107、旧客户端跨设备 6/6 等记录，只能按原记录的提交／工作树时点、命令和覆盖范围解释。本次未复跑，也未验证所提交 Android so 与当前 Rust 源码的对应关系；恢复集成测试文件本身不等于运行过真实入口。本报告不撤销已经实际取得的历史证据，也不把历史总数转写为本次修复验证通过。

测试源码检查已识别出几种明确覆盖错位：门禁测试走包装器而 FFI 绕过包装器；额度错误测试读裸 TLS 帧而生产客户端丢帧；协调断线测试手造 registry 原因而 FFI 发 receiver_failed；目录测试在 Offer 前改设置而漏掉 pending consent；并发幂等测试等两个受理都返回后才 poll；过期横幅测试使用本地 Fake 时间；升级 fixture 只覆盖 v1，不含 wave-28 的 41B 默认 journal。这些是测试设计范围的静态结论，**不是宣称这些测试本次运行失败**。

未发现本波新增 TURN／relay、HTTP 文件上传、云端暂存或第三设备代传的正常代码路径。新增关闭通知仅为端点间控制帧，文件仍经真实 QUIC 身份与既有策略检查。没有抓包／真实路径实验，不能宣称证明所有部署网络无代理或中继。没有从本次修复差异确认新的路径越界／符号链接绕过；接收继续使用既有 LocalSaveTarget 路径，既有平台限制也没有因此获得重新验收。

## 总结论

**不通过。** 原 21 项的复核计数为：**已关闭 6 项、部分关闭 13 项、未关闭 2 项**；新增问题 **5 项：P0 × 0、P1 × 0、P2 × 5**。

| 原分组 | 已关闭 | 部分关闭 | 未关闭 | 合计 |
|---|---:|---:|---:|---:|
| P1 | 1 | 4 | 1 | 6 |
| P2 | 5 | 9 | 1 | 15 |
| 合计 | 6 | 13 | 2 | 21 |

不通过的决定性依据是：生产 owner 门禁仍未安装，错误兑换仍可能先 Commit／落簿并消耗唯一额度；协调断线仍终止健康直连的任务状态。接收监听可被单次失败握手结束、空闲关闭仍缺一个方向、GUI 未接未加入码取消、完成事件被过滤、pending consent 保存目录与期限不同步，以及固定总时限误断健康长传输，也尚未形成完整修复。真实入口、事务额度、异步同意、迁移原子性和缓存上限等已有实质改善，但不足以支持“21 项全部关闭且无新问题”的声明。

后续收口应先修正生产跨层接线和完整终态契约，再针对本报告已明确的触发边界补充验证。**本轮构建／测试执行数量均为 0，遵守只读评审约束。**
