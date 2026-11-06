# AK-sending 第三十一波修复第三轮复核报告

评审日期：2026-11-07。评审对象：`df7d4ee01c961f38faa98f278f0ab132bdb310c8`；差异基线：`5dade176c5845c555f48400107ca987d8629f297`。上轮依据：[wave-30 第二轮复核报告](codex-recheck2-2026-11-07-wave30.md)；追溯依据：[wave-29 第一轮复核报告](codex-recheck-2026-11-07-wave29.md)、[wave-28 原始评审报告](codex-review-2026-11-06-wave28.md)。源码行号均对应本次评审提交。

本次已阅读 README、HANDOFF、ROADMAP、ADR-001、上轮发现、修复差异、相关生产代码及测试源码；按 AGENTS.md 的并行规则，分别审查 Rust FFI、Dart 服务、Dart 绑定与契约，由主评审收口跨层时序和历史剩余范围。开始时 HEAD 为上述提交，工作树干净。**只读审查代码与既有文档，唯一写入为本报告；构建、测试、仓库检查、语法检查、设备实验、网络实验及部署操作均未执行。**

判定口径：“已关闭”表示源码关闭了对应原触发及上轮要求的剩余路径，不代表测试实际通过或真机验收完成；“部分”保留仍未满足的原范围。“是否通过”单独按是否仍确认影响本版用户主线路径的正确性或安全性缺陷判定，不以历史编号的 P1/P2 等级或“部分”数量机械代替。P1-03 与 R29-P2-01 等关联条目仍分别跟踪，但不把同一根因重复当作独立缺陷。

## 逐项复核表

### 上轮新增的 5 项 R30 发现

| 编号与原触发 | 本轮判定 | 源码证据与修复范围 |
|---|---|---|
| **R30-P1-01：接收等待在 dispose 后轮询已释放原生 engine** | **已关闭** | `flutter_app/lib/services/ffi_simple_transfer_service.dart:319`、`:344` 为 done/connection 订阅增加 onDone；`:354` 的定时 await 返回后先在 `:358` 检查 isDisposed，再于 `:361` 调 drainProgress；经流关闭退出循环时也在 `:379` 抛 StateError。`simple_transfer_service.dart:375` 先置 disposed、取消 pump、关闭流，`:665` 的 drainProgress 本身拒绝已释放服务；adapter `:209` 先使借用者失效，`:210` 后调用 native owner。原“接收进行中调用公开 service.dispose，后续 tick 经旧指针进入 FFI”的路径被切断。接收超时清理跨 await 后进入 `_engine.disconnect`，仍经其存活门禁，不恢复该路径。结论限定于生产采用的借用者先失效、owner 后释放顺序。 |
| **R30-P2-01：发送续期丢失事件泵已消费的进度** | **已关闭** | `simple_transfer_service.dart:577` 在每个 sendFiles waiter 内创建 lastSeen，`:584` 的 onPoll 闭包跨轮询复用；`:704` 的 `_progressAdvancedSince` 比较全局累计 tick 与本 waiter 上次观察值，再更新账本。50ms pump 先消费的行已计入累计 tick，200ms waiter 下次仍能识别增长。比较基线不再在每次 drain 前重置；不同发送 waiter 也不共享可互相覆盖的基线。 |
| **R30-P2-02：Sender 生码、Receiver 加入时接收续期缺 code_id** | **已关闭** | `crates/ak-ffi/src/task.rs:2734` 的共用接收回调经 `conn_of_code` 取得 conn_id，在 `:2741` 用它输出 progress.job；发送侧 `:3213`、`:3221` 同样使用 conn_id。双方连接 id 均为配对事务 id（`:1414`、`:2988`）。adapter `ffi_simple_transfer_service.dart:277`、`:283` 只按请求 connId 续期，`:346` 的观察基线跨轮询保存，不再依赖本机曾生码。owner/joiner 的 connected（`task.rs:1441`、`:3035`）及 join 结果（`:3052`）均增加 code_id；Dart `task_events.dart:99`、`simple_transfer_service.dart:466`、`:980` 同步解析与登记。正常开始接收前的指纹准入要求连接已存在，连接记录又不移除，故接收回调的 code_id 兜底分支不是已确认的合法在途路径。 |
| **R30-P2-03：终态先到、incoming 后到留下永久待同意横幅** | **已关闭** | `task.rs:833` 获取 pending 后，在 `:842` 复查连接存活、`:854` 插入、`:884` 发布 incoming；这些操作位于同一 pending 临界区。关闭端 `:753` 获取同锁，在移除请求后于 `:769` 发布 request_closed，再 send(false)。`task_disconnect` 于 `:2252` 释放 conns，之后才于 `:2259` 进入 pending，所阅生产路径未见反向嵌套造成的新死锁。Dart `task_events.dart:143` 增加 request_closed 解析，service `:1065` 处理关闭，`:923` 保留 FIFO 256 个请求关闭记录，`:935` 同时检查请求记录和连接终态，`:1013` 拒绝迟到 incoming；consent_expired 也进入关闭记录。Native 与 Dart 两侧合起来关闭了上轮时序。原子性限定见后表。 |
| **R30-P3-01：handshake_failed 文档形状与 native 不一致** | **已关闭** | `task.rs:2804`、`docs/contracts/ffi-v2.md:433`、`:442` 与 `task_events.dart:131`、`:375` 均使用 code_id 字符串、固定 reason，且不要求 conn。`task.rs:2913` 保留固定两种原因，失败握手后继续 listener；Dart 服务不把该事件当连接终态。原“整数 code/必有 conn”的错误契约已移除。 |

### 上轮仍为“部分”的 7 项历史条目

| 编号与上轮剩余范围 | 本轮判定 | 当前证据、剩余范围及是否决定性 |
|---|---|---|
| **P1-03：单次额度与竞争败者错误送达；剩余为 Code 域归因** | **部分** | `crates/ak-signal/src/server_v2/store.rs:980`、`:1048` 的事务额度复查保留，败者在 `:1054` 转 aborted；同版服务端 `engine.rs:1867` 使用 Pairing/pairing_id，`:1874`、`:1875` 双侧送达。旧 Code 域分支仍由 `crates/ak-session/src/pairing/orchestrator.rs:1621` 只匹配查询的 Err(ClientError::Pair)，无法识别正常返回的 Ok(Aborted)。本波未改该处。**非决定性**：上轮已明确原越额不变量与同版竞争错误路径关闭，残项是旧服务端 Code 域竞争错误的及时归因，不能改述为本版仍可多次 Commit。与 R29-P2-01 同一残留根因。 |
| **P2-03：接收方主动断开，空闲纯发送方不能即时获知** | **部分** | `task.rs:2393` 仍只尝试通过认证 QUIC 数据端点发 TASK-CLOSE；Sender 创建/加入分支 `:2541`、`:2959` 无数据 listener。`task.rs:2227` 与 `ffi-v2.md:460`、`:467` 明示纯发送方在下次发送失败时感知。**当前可接受的非决定性边界**：存在状态感知延迟，未达到原“双端即时收敛”范围，故不能标为已关闭；但下一次实际发送仍须拨号、验证真实对端并取得对端协议响应，不会凭本地旧的 connected 行宣称文件送达。没有本波证据表明这一边界造成健康在途传输误切或绕过同意。 |
| **P2-04：未加入码的 TTL 本地回收、服务器撤码确认** | **部分** | GUI 取消继续经 `pairing_flow_controller.dart:159`、`:171` 调 disconnect(codeId)，native `task.rs:2356` 取消 listener/echo 任务。但 `:2366` 异步撤码、`:2367` 丢弃结果、`:2370` 先发 code_cancelled，仍不能据 API 返回断言服务器已确认撤销。`crates/ak-session/src/pairing/task.rs:668` 的 ActiveTask 无到期时间，`:797` 登记后未增加 TTL 驱动的本地资源回收。**本轮非决定性，但确实未完成**：600 秒服务器 TTL、显式本地取消与 teardown 尽力清理保留；残项涉及到期资源、控制服务失败后的撤销确认，不是本轮再次证实的正常传输错盘、错误完成或授权绕过。不得把 code_cancelled 解释为服务器确认，也不得宣称长时间反复生码的资源生命周期已验收。 |
| **P2-06：ReceiveSpec 与 incoming.saveDir 二次读取错位** | **已关闭** | `task.rs:2819` 在每次接受连接后只读取一次 out_dir，`:2827` 克隆给 ReceiveSpec，`:2838` 捕获到本次 consent 闭包，`:2844` 传入 register_consent，`:881` 输出 incoming.saveDir；完成时 `:2862` 使用同一 out_dir。`crates/ak-engine/src/lib.rs:2924` 开保存目标，`:3555` 的 consent 仍只是 bool；未伪称在接受决定时切换目录。`ffi-v2.md:446`、`:459` 已明确采用原报告允许的“固定目标并展示真实目录”替代语义。UI `incoming_requests_host.dart:188`、`:234` 均优先用请求的固定 saveDir，因此 TLS accept→Offer 或 pending 期间 A→B 不再让界面显示 B、实际写 A。新增 done.saveDir 在 Dart 类型边界丢失另计 R31-P3-01，不重新打开已修好的用户错位路径。 |
| **P2-10：长传输误切、取消失败措辞及停止确认** | **部分** | 原有两条健康长传输误切分别随 R30-P2-01/02 关闭，接收生命周期问题随 R30-P1-01 关闭；取消抛错时仍明确提示可能后台运行。剩余是 `simple_transfer_service.dart:587`、`:597` 与 adapter `:286`、`:297` 仍把 disconnect 返回表述为“已取消”。native `task.rs:2263`、`:2267` 请求 abort 后在 `:2305` 返回，没有等待 task join 的停止完成确认。**不再是上轮决定性问题**：当前默认路径有进度即可续期；残项是取消受理与取消完成的语义边界，本次未确认其导致取消后持续后台传输的独立主线故障。保留部分，不能把异步 abort 受理扩大为停止完成已确认。 |
| **P2-11：原生到期/关闭与横幅生命周期不同步** | **已关闭** | 原生 consent_expired 在 `task.rs:899` 保留；request_closed 已完成 typed 解析和 service 路由，两个请求终态均写入有界关闭记录，连接终态也关闭 pending 并拒绝迟到 incoming。结合 R30-P2-03 的同锁登记/发布，原生已关闭而 UI 重开永久待同意横幅的原时序已消除。无需恢复以 UI Timer 猜测原生期限。 |
| **R29-P2-01：Commit 满额错误进入真实业务流；剩余为旧 Code 域** | **部分** | 与 P1-03 同一残留：`orchestrator.rs:1621` 未识别 Ok(Aborted)，而 `engine.rs:2033`、`:2047` 会正常返回仍在库中的 aborted 事务。连接仍发送 Code 域竞争错误的旧服务端时，败者可能继续等到既有超时。**非决定性**：本版 Pairing 域路径继续成立；本次不把旧服务端兼容分支未补齐升级为同版主线回退，也不宣称所有版本组合均已兼容。 |

这里的“非决定性”并非因为该项不在本轮修复声明内，而是依据上轮已经限定的剩余触发、当前生产路径与实际影响范围。P2-03 的状态延迟、P2-04 的资源/撤销确认和 P2-10 的取消完成确认仍是后续工作；通过本轮复核不等于批准“全部历史问题均已关闭”的声明。

### 其余 19 项历史关闭状态的保持性核对

本表结合差异和相关入口核对既有关闭状态，未重新执行原验证矩阵。

| 编号 | 上轮 → 本轮 | 保持性证据 |
|---|---|---|
| P1-01：真实 GUI 入口接线 | 已关闭 → **已关闭** | `flutter_app/lib/main.dart:100`、`:101`、`:105` 仍为真实 task bindings、adapter 和 owner.dispose，没有恢复无条件 Fake；新增存活门禁只增强该路径。 |
| P1-02：任务码格式 | 已关闭 → **已关闭** | ffi-v2 的 join/create/connected/list 示例仍为 AKI1；`home_page.dart:271`、`:281` 保留完整码输入，未恢复 AKT1 或八位码限制。 |
| P1-04：单次握手失败终结 listener | 已关闭 → **已关闭** | `task.rs:2774` 保留已验证对端指纹准入，`:2782` 对 Unauthorized 继续，`:2791` 对单次握手失败记事件后继续；本波只对该事件字段命名作对齐。 |
| P1-05：生产 Commit 前角色/endpoint 门禁 | 已关闭 → **已关闭** | `task.rs:1287` 安装门禁先于 `:1327` 启动 owner loop；session `task.rs:986`、`:989`、`:992` 的 endpoint/方向检查未改。仍不把“省略角色”泛化为全部拒绝。 |
| P1-06：同一 connections 流多次 listen 的快照 | 已关闭 → **已关闭** | `simple_transfer_service.dart:504` 的 Stream.multi 保留每个 listener 独立订阅与当前快照；新增终态逻辑未撤掉它。 |
| P2-01：request_id 原子受理与 op 可见性 | 已关闭 → **已关闭** | `task.rs:1883` 持 request registry 锁，`:1913` 先插入 Running op，`:1915` 再发布 id；操作域、连接及 canonical spec 去重保留。 |
| P2-02：并发首次初始化 | 已关闭 → **已关闭** | `task.rs:1222` 的异步初始化锁、`:1223` 的锁内二次查栈、`:1231` 的停止检查仍在，本波未改变初始化流程。 |
| P2-05：完成后仍留失效发送入口 | 已关闭 → **已关闭** | `task.rs:2868`、`:3132` 保留完成/断开事实；Dart service `:1029` 收敛 done，adapter `ffi_simple_transfer_service.dart:236` 继续过滤 done/disconnected。 |
| P2-07：同步 consent 阻塞 worker | 已关闭 → **已关闭** | `register_consent` 仍返回 Pending oneshot，`task.rs:891` 为独立异步到期任务；新增互斥仅包围有限登记、查询及入队，没有恢复等待人工同意的同步阻塞。 |
| P2-08：协调断线误杀健康直连 | 已关闭 → **已关闭** | 本波未恢复 session network watcher 或 FFI 的协调断线→数据面失败接线；`simple_transfer_service.dart:872` 保留数据面终态理由表。 |
| P2-09：失败探测无条件变 connected | 已关闭 → **已关闭** | `task.rs:1475` 的 settle_probe_state 及 owner/join 调用保留；Dart `simple_transfer_service.dart:987` 的 reachable 真值归并未回退。探测仍不等于身份或路径授权证明。 |
| P2-12：UDP echo 互相应答 | 已关闭 → **已关闭** | `task.rs:1616` 继续忽略全零 nonce 的回复；本波未改响应形状与来源预算。 |
| P2-13：schema 迁移非原子 | 已关闭 → **已关闭** | `crates/ak-signal/src/server_v2/store.rs:1342` 的单事务迁移未改，未引入依赖或 schema 升级。 |
| P2-14：历史 41B claim 幂等重放 | 已关闭 → **已关闭** | `store.rs:1556` 的窄范围 canonical 比较未改；默认历史体归一与非默认参数冲突边界保持。 |
| P2-15：endpoint 缓存无界积存 | 已关闭 → **已关闭** | `crates/ak-session/src/pairing/client.rs:166` 的 256 上限、`:227` 的淘汰及既有错误/断线/消费清理未改。保留原“不承诺每个终态立即回收”的边界。 |
| R29-P2-02：36B 修复破坏历史 41B 默认记录 | 已关闭 → **已关闭** | 与 P2-14 共用未改变的兼容比较实现；没有新的编码改动。 |
| R29-P2-03：提前发布 op_id 导致立即 poll 得 -4 | 已关闭 → **已关闭** | 与 P2-01 共用 `task.rs:1913` 先登记 op、`:1915` 后发布 id 的临界区，未恢复发布窗口。 |
| R29-P2-04：角色取消选择后访问空 Set.first | 已关闭 → **已关闭** | `home_page.dart:327` 保留 selection.isEmpty 分支，UI 本轮未改。 |
| R29-P2-05：offer gate 捕获完整 TaskPairing 形成 Arc 环 | 已关闭 → **已关闭** | session `task.rs:958`、`:961` 仍只捕获任务表；本波没有修改该闭包或重新捕获完整 pairing。 |

### 两侧契约与修复间一致性

| 核对点 | 本轮结论 |
|---|---|
| 进度 job、请求 job 与 code_id | 发送/接收的 **progress.job 是 connId**；incoming_request/consent_expired/request_closed 的 **job 是每次同意请求的独立 id**；code_id 是任务码关联 id。三者用途不可互换。两侧生产进度与 Dart waiter 已统一，connected/join 的 code_id 是补全关联事实，不再承担接收续期必需的查表前提。 |
| connected / join 字段 | native 两端 connected、join 结果、ffi-v2 和 Dart/Fake 的 code_id 字段名一致；Dart 按可选字段接收，以免缺字段直接解码失败。这不等于旧 native 的 code-id 进度与新 Dart 的 conn-id 续期仍可混用。 |
| handshake_failed | native、冻结示例与 typed parser 一致：code_id 为字符串，无 conn，reason 固定；仅表示单次握手失败，不终结整个监听器或已配对任务。 |
| disconnected / done 终态 | native、ffi-v2、typed model 与 service allowlist 均采用 local/peer/receiver_failed/done；done 收敛连接并关闭活动入口，协调断线不进入该终态表。 |
| 请求终态原子性 | 同一 pending 锁排除了 **request_closed→同一请求 incoming_request**。但存活复查释放 conns 锁后，断开仍可先发布 disconnected，因此 **disconnected→incoming_request→request_closed** 仍是合法时序；Dart 的连接终态门禁负责拒绝该 incoming。不能把修复扩写成“所有终态与 incoming 逆序都从 Rust 消失”。 |
| saveDir 语义 | ReceiveSpec、原生 incoming.saveDir、原生接收 done.saveDir 使用同一固定目录；改设置只影响后续传输。等待和完成横幅均用保存下来的请求目录，满足原报告替代方案。**Dart done 类型仍丢字段，见 R31-P3-01。** 发送侧 done（`task.rs:3123`）本来不含接收方本地目录，该字段应明确为接收侧提供、类型上可选。 |
| 注释与文档残差 | `task_ffi.dart:978` 仍把接收进度写成 code id；`task_events.dart:423`、`simple_transfer_service.dart:1011` 仍称 native 登记/发布不在同一临界区；部分 saveDir 注释仍称 Offer 时读取；`ffi-v2.md:440` 仍称 UI 本地计时兜底，而实际已无该 Timer。这些文字应同步，未据此额外虚增主线故障计数。 |

### 新增测试源码的覆盖边界（全部未执行）

| 修复声明 | 源码提供的覆盖与不能据此作出的声明 |
|---|---|
| dispose 后调用计数冻结 | `flutter_app/test/ffi_simple_transfer_service_test.dart:447` 在接收等待期间 dispose，要求 StateError，并比较后续 Fake 调用计数；`simple_transfer_service_test.dart:789` 独立覆盖 drainProgress 门禁。adapter 测试的 engineOwner 是空函数，属于 Fake 调用路径覆盖，不能称真实 native 释放后内存安全动态验证。 |
| 发送续期的生产周期 | `simple_transfer_service_test.dart:821` 的 sender 保留 50ms pump、200ms waiter，25ms 注入进度并持续 600ms，改善了上轮关闭 pump 的覆盖错位；没有屏障或消费者计数保证每一轮必由 pump 先消费，关闭依据还包括跨轮询账本的源码。 |
| 接收者作为加入方 | adapter 测试 `ffi_simple_transfer_service_test.dart:486` 覆盖 Sender 生码、Receiver 加入，但使用两个 Fake runtime。Rust `task.rs:5912` 为真实双引擎传输测试源码，检查双侧 conn 进度键，但方向仍是 Receiver 生码、Sender 加入；不能称其已直接动态验证上轮相反方向。两个入口共用同一生产 receiver 的源码是结论的一部分。 |
| 双侧共享 code_id | `task.rs:5873` 检查 owner/joiner 两条 connected 和 join 结果；Dart 模型测试检查字段存在与缺省。均未执行。 |
| 终态先到与迟到 incoming | Dart `simple_transfer_service_test.dart:932` 覆盖 disconnected→request_closed→late incoming，`:981` 另覆仅连接终态门禁；未独立穷尽仍 live 连接上的请求关闭记录分支或第 257 条淘汰。Rust `task.rs:6003` 覆盖预先断开；`:6078` 的 64 轮是普通线程调度压力源码，并非指定窗口的确定性交错证明，且存在 R31-P3-02 的测试误报失败路径。 |
| 不可变保存目标 | `task.rs:6181` 在 pending consent 期间 A→B，核对 incoming、实际文件内容/落点与 done 都是 A；未单独阻塞 TLS accept→Offer 窗口，但生产代码已取消第二次全局读取。Fake 手动给 saveDir 的 UI 测试仍不能单独证明实际落盘。 |

## 新发现（分级）

本轮新增 **2 项：P0 × 0、P1 × 0、P2 × 0、P3 × 2**。均为源码确认的接口或测试缺陷，未动态复现；没有将历史未完成项重新编号重复计数。

### R31-P3-01：新增接收 done.saveDir 在 Dart typed 事件边界被丢弃

- **位置**：`flutter_app/packages/ak_ffi_dart/lib/src/model/task_events.dart:118`、`:325`、`:338`；`flutter_app/packages/ak_ffi_dart/lib/src/task_ffi.dart:967`；`flutter_app/packages/ak_ffi_dart/lib/src/fake_task_ffi.dart:603`。对照 `crates/ak-ffi/src/task.rs:2860` 和 `docs/contracts/ffi-v2.md:430`、`:444`。
- **触发**：真实接收成功，native 发布含实际固定目录的 done；`TaskRuntimeFfi.pollEvent()` 将 JSON 转为 TaskDoneEvent。
- **证据**：done 解析分支只取 conn/files/bytes；TaskDoneEvent 的构造器、属性及 fields 均无 saveDir。新增字段因此在 Dart 消费者取得 typed event 之前丢失。Fake emitDone 也没有该字段，既有 done 模型测试及本轮新增模型测试未覆盖它。
- **影响与分级**：冻结契约新增的完成结果字段无法经 Dart SDK/服务 doneEvents 读取，“新 done 字段已贯通两侧”不能成立。**当前 GUI 不因此再度错盘或错报目录**：完成横幅仍取 incoming_request.saveDir，且本波已将它与实际落盘目标固定为同一值。故列 P3，不作为主线不通过依据，也不把原 P2-06 重新判为未修复。
- **修复建议**：TaskDoneEvent 增加可选 saveDir，补解析、fields 与 Fake/模型覆盖；契约注明字段由接收侧提供。发送侧 `task.rs:3123` 的 done 不含该字段，不应强制发送方提供接收方本地路径。
- **置信度**：高；字段丢失可由源码直接确定，未执行测试。

### R31-P3-02：新增 64 轮竞态测试会把合法到期误判为关闭顺序失败

- **位置**：`crates/ak-ffi/src/task.rs:6078`、`:6130`、`:6136`、`:6160`；到期逻辑 `:891`、`:893`、`:899`，断开清理 `:753`、`:757`。
- **触发时序**：registrar 已登记并发布 incoming；closer 线程尚未获调度；测试设置的 **25ms** 窗口先到期，timer 移除 pending 并发 consent_expired；随后 closer 发布 disconnected，因 pending 已空而不再发 request_closed。
- **证据**：测试没有保证 closer 先于到期任务运行的同步机制，却在 `:6160` 只接受相同 job 的 request_closed。上述顺序是生产逻辑允许且已正确终结请求的顺序，断言会因缺少 request_closed 错误失败。测试只 join 两个线程，也未同步等待独立到期任务发布终态。
- **影响与分级**：新增回归门槛在负载或线程调度延迟下可能误报失败，影响验证可信度；不是产品登记/关闭逻辑失败，列 P3。**本次没有运行测试、没有观察到实际红测**，不能把这里写成“64 轮测试已失败”。
- **修复建议**：要么通过同步机制控制/隔离到期参与，专测登记与断开的顺序；要么等待同一 job 的任一种合法请求终态，并验证它晚于 incoming。仅把 consent_expired 加入断言允许集仍不充分：到期任务可能已移除 pending、尚未发布事件，而当前 snapshot 已开始读取。
- **置信度**：高；来自测试中明确存在的独立到期任务与过窄断言，未动态复现。

## 总结论

**通过。** 判定限于本次对 `df7d4ee` 的代码与文档复核：上轮决定性缺陷 R30-P1-01（接收 dispose 后悬空原生指针）及 R30-P2-01/02（两条健康长传输误切路径）的原触发均已由源码关闭；请求登记/终态竞态和保存目录错位也形成了跨层闭合。未确认本波修复引入新的、影响本版用户主线路径的正确性或安全性缺陷。

| 跟踪分组 | 已关闭 | 部分 | 未关闭 | 合计 |
|---|---:|---:|---:|---:|
| 原 P1 | 5 | 1 | 0 | 6 |
| 原 P2 | 12 | 3 | 0 | 15 |
| 原发现合计 | 17 | 4 | 0 | 21 |
| 历史新增 R29 | 4 | 1 | 0 | 5 |
| 上轮所称历史 26 项 | **21** | **5** | **0** | **26** |
| 上轮新增 R30 | **5** | **0** | **0** | **5** |
| 本次复核的既有条目总计 | **26** | **5** | **0** | **31** |

本轮另新增 **2 项 P3**（done.saveDir 的 Dart 类型遗漏、竞态测试可能误报失败），均尚未修复，**不混入上表 31 项历史复核状态**。若将新发现一并入账，总计 33 项，其中已关闭 26、部分 5、新增待修复 2。

与上轮相比，历史 26 项从 19 已关闭/7 部分推进为 **21 已关闭/5 部分**，变化是 P2-06 与 P2-11 关闭；上轮新增 5 项 R30 均关闭。仍为部分的 P1-03/R29-P2-01 指向同一个旧服务端错误归因残项；P2-03 是已披露的空闲纯发送方断线感知延迟；P2-04 是 TTL 本地回收与服务器撤销确认；P2-10 是取消停止确认语义。按上轮原文限定，不能据这些残项断言额度越界、同版错误丢帧或健康传输误切仍然存在，也不能因本轮通过而把这些残项写成全部完成。

证据边界：提交说明中的 workspace 1123/0、Dart 139/139、Flutter 133/133、clippy/契约通过、Windows/Android 库重建及 APK 暂存，均为提交者的历史验证声明，**本次没有复跑或独立验证**。本次也没有验证提交的两份 Android so 与源码对应关系、平台构建、双机 GUI、真实长传输、真实释放后访问或抓包。README/HANDOFF/ROADMAP 仍保留 wave-29“进行中/未提交”的滞后状态，不应用作 wave-31 的验收证据。通过只读复核不等于发布验收或平台实测通过。

本次差异未确认新增 TURN/relay、HTTP 文件上传、云端暂存、第三设备代转或隐藏代理回退；接收继续沿用实际 TLS 指纹准入与引擎既有保存目标、路径和符号链接防护。未把探测结果、Observation 布尔值或枚举当作身份/路径的密码学证明。该判断只覆盖所阅差异与调用链，不是无中继抓包证明或对所有平台存储防护的重新验收。**本次构建执行数 0，测试执行数 0，仓库/语法检查执行数 0；仅新增本报告。**
