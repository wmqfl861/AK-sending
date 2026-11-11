# AK-sending 第二十八波内部质量与正确性终审

## 执行摘要

**结论：本波不宜按“v1 极简配对 UX 已完成”通过终审。确认 21 项发现：P0 × 0、P1 × 6、P2 × 15。**

评审对象为 `5a5dac1..7458e9f735ae25063a99c037efb0958dd9586b16`，即第二十八波提交 `7458e9f`。本报告于 2026-11-06 编写，按协议/存储、会话/引擎、Dart/UI 分工只读审查，再交叉复核 FFI 和实际入口。只写入本报告，没有修改产品代码或契约，也没有执行构建、测试、仓库检查、语法检查、设备实验或网络实验。

最直接的发布阻断是 **Flutter 的实际 `main()` 仍无条件构造本地 Fake**，替换掉了旧真实服务。即使接上已经存在的真实适配器，UI 八位码校验仍不接受 Rust 实际生成的 AKI1 码；接收方生成码的跨设备加入又会默认选择错误方向；真实连接流缺少逐订阅者快照，稳定连接的详情页可能直接显示“已断开”。这些是彼此独立的集成缺陷，不能以底层测试通过替代修复。

安全与正确性方面，**单次兑换额度没有在最终 Commit 事务重新检查**，两个预先挂起的兑换可以全部提交；**任务接收器接受任意自签身份**，没有将真实入站公钥绑定到本次配对对端。接收同意本身不等于身份认证：当前提示可以被归到合法任务连接，却实际来自另一个设备。除此之外，FFI 幂等、任务关闭、并发初始化、接收同意的调度方式及升级迁移也有可复核缺口。

已核查到的正面结果包括：endpoint 的 1..=512B 限制在协议编解码两端及会话解释层执行；无扩展的旧字段帧仍按原布局编码；撤销与最终 Commit 有事务化状态复查；Offer 拒绝发生在 Accept、用户文件/分片及检查点记录写入之前；新版 Dart 服务的主要异步轮询保留了 wave-26 P1-05 的 dispose 前置失效模式。**没有确认新的 P0 级 ABI 错配或内存破坏路径，也没有发现新增 TURN/relay 文件数据回退。**这些是源码结论，不是动态安全证明。

用户提供的 **workspace 1085/0、Dart 128/128、Flutter 86/86、升级服务器旧客户端跨设备兼容 6/6** 均按既有验证记录处理，本轮没有复跑。尤其旧 invite 跨设备兼容不能证明新 GUI 的 main→Dart→FFI→任务码链路；正常新请求兼容也不能覆盖旧数据库幂等记录跨版本重放。本波删除了三个真实集成测试文件，而新增 UI/服务测试主要使用 Fake，正好遗漏上述实际接线和时序问题。

本报告的 P1 表示应在本波收口前修复的安全边界或主要用户流程阻断；P2 表示需要明确修复的并发、生命周期、恢复或状态正确性问题。每项置信度针对代码结论；没有将未执行的建议验证写成已复现。部分真实服务问题目前被 Fake 入口遮蔽，修复入口后仍会独立发生。

## P0 发现

**未确认 P0 发现。** 八个 `ak_task_*` 的 C ABI 形状与 Dart 绑定静态对照未发现上一轮那类参数数量错配。指针有效性、真实容量及 free 后不得继续使用仍属于调用方契约；本结论不是对任意调用方误用的内存安全保证。

## P1 发现（6 项）

### P1-01：应用入口仍选择本地 Fake，真实 GUI 配对和传输没有接线

- **位置**：`flutter_app/lib/main.dart:21`、`:24`、`:28`；`flutter_app/lib/services/simple_transfer_fake.dart:113`；真实适配器在 `flutter_app/lib/services/ffi_simple_transfer_service.dart:80`。
- **证据与触发**：本波将原来的 `FfiTransferService()` 替换为无条件 `LocalSimpleTransferService()`，入口注释仍要求后续替换。Fake 的码由本地计数生成，加入只查同一服务实例的待配对码，发送/探测完成依赖测试驱动，未调用真实引擎。普通启动应用即进入该路径。
- **影响**：新版 GUI 的核心功能整体退化为本地演示，不能跨设备配对或传输。已有 Rust、Dart adapter 的实现和验证不能使这条产品入口自动成立。
- **修复建议**：完成真实 engine 所有者→`AkTaskFfiBindings`→`FfiSimpleTransferService`→应用的接线，包括平台身份目录初始化、启动失败状态和释放顺序；Fake 仅由测试或明确开发入口注入。Android 需要恢复旧服务已有的 `path_provider` 初始化能力，新适配器的环境变量/相对路径回退不能代替它。
- **覆盖缺口**：新版 widget 测试通过 `flutter_app/test/w28_test_helpers.dart:20` 注入 Fake；`ffi_simple_transfer_service_test.dart` 又使用 `FakeAkTaskCalls`。需要以实际 main 和真实绑定覆盖最小双端流程，本轮未执行。
- **置信度**：高。

### P1-02：UI 八位码校验与真实 AKI1 码不兼容，任何真实任务码都无法通过输码入口

- **位置**：`flutter_app/lib/ui/pages/home_page.dart:267`、`:276`、`:290`；`flutter_app/lib/ui/service/pairing_flow_controller.dart:90`；`crates/ak-ffi/src/task.rs:1939`、`:1977`；`crates/ak-session/src/pairing/orchestrator.rs:133`。
- **证据与触发**：输入框最多九字符，提交条件和控制器都要求规范化后恰好八位；Rust 通过真实 InviteCode 返回 AKI1 前缀加 160bit 随机秘密的完整展示码。该真实码远超输入上限，无法录入或通过控制器。Dart/FFI 契约里残留的 AKT1 示例也不符合 UI 八位限制。
- **影响**：即使修复 P1-01，真实发起方生成的码仍无法由另一端 GUI 加入。这不是大小写或连字符规范化能解决的问题。
- **修复建议**：确定唯一的任务码格式契约，由生成、展示、复制、输入、规范化和服务调用共同遵守。若继续使用 AKI1，应直接支持完整格式；若产品确需短码，应另行明确熵、在线防猜测和协议方案，不能只截短现有码。
- **覆盖缺口**：Fake 生成八位码，真实 adapter Fake 测试又断言 AKT1，分别验证了不同协议。应将真实 Rust 生成码用于 UI 输码场景。
- **置信度**：高。

### P1-03：单次兑换只在 begin 时计数，多个 pending 可越过 `max_redeems=1` 全部提交

- **位置**：`crates/ak-signal/src/server_v2/store.rs:760`、`:1007`、`:1047`；生产处理入口 `crates/ak-signal/src/server_v2/engine.rs:1455`；契约 `docs/contracts/pairing-flow.md:82`、`docs/contracts/protocol-v2.md:264`。
- **证据与触发**：`begin_pairing` 统计 committed/applied/ready，允许不计额度的 pending；最终 `store_prepared_and_try_commit` 只重新检查码 active/expiry 和 owner relation 配额，然后直接写 committed，未复查 `max_redeems`。B、C 在第一个提交前先各自进入 pending，之后即使依次提交，也都能成功。
- **影响**：权威服务器没有实现“一码恰好双方”。不同客户端可为同一单次码建立多条已提交配对，客户端缓存或 UI 的额外限制不能修复服务器不变量。
- **修复建议**：在最终 Commit 的同一写事务中重新读取额度并计数，超过额度则终止该未提交配对并返回明确结果。保留 pending/aborted 不消耗额度的约定；不能仅在 begin 时预占后永不释放，也不能提交后补减计数。
- **覆盖缺口**：`engine.rs:3106` 的 single-redeem 测试先完成第一笔再发第二笔，未覆盖两个 pending 后竞争 Commit。建议同时覆盖 cap>1、同 request_id 重放以及撤销与额度竞争。
- **置信度**：高，SQL 与生产控制流即可确定。

### P1-04：任务接收器未绑定配对公钥，任意身份可抢占接收器并冒用合法连接的提示

- **位置**：`crates/ak-ffi/src/task.rs:2062`、`:2065`、`:590`、`:605`；`crates/ak-engine/src/lib.rs:675`、`:2796`；`crates/ak-net/src/lib.rs:532`；`crates/ak-net/src/tls.rs:167`。
- **证据与触发**：新接收路径显式设置 `spec.authorize = Box::new(|_| true)`，接受任意合法自签客户端证书后进入旧 Offer 流程，没有本任务的准入校验。ConsentInfo 不包含实际发送者公钥；FFI 按接收器的 code_id 查连接并把提示附到那个 conn。能够触达端口的另一设备无需任务码即可先完成 TLS、发 Offer，或占住唯一接收器。
- **影响**：用户可能认为在同已配对设备交互，却接受另一身份的文件；拒绝或不发送 Offer 也能妨碍合法发送方使用接收器。这里仍需要用户接受才会进入用户文件写入，不能夸大为无提示自动落盘。源码中“stray dial 无法 TLS”的理由不成立：发送侧钉扎保护发送方，不能替代接收侧鉴权。
- **修复建议**：将实际 TLS peer fingerprint 与该任务的已验证配对身份绑定，未完成配对时不得放行陌生身份进入 Offer。Consent 请求也应关联真实已授权对端和任务，不能仅凭监听端口所属 code_id 赋予归属。陌生拨号失败不应耗尽合法任务的接收能力。
- **覆盖缺口**：应覆盖合法 A/B 配对后 C 抢先拨号、配对完成前 C 拨号、C 发送 Offer 被零落盘拒绝，以及合法 B 仍可继续。现有接受/拒绝正例未检验第三方身份。
- **置信度**：高。违反 ADR-001 的端点身份与任务授权边界。

### P1-05：跨设备加入接收方生成的码会选择错误方向，失败发生在唯一兑换和落簿之后

- **位置**：`crates/ak-ffi/src/task.rs:2146`、`:2153`；`flutter_app/packages/ak_ffi_dart/lib/src/task_ffi.dart:214`、`:235`；`flutter_app/lib/services/simple_transfer_service.dart:343`；`crates/ak-session/src/pairing/task.rs:818`、`:848`、`:854`；`crates/ak-session/src/pairing/orchestrator.rs:438`、`:472`、`:483`。
- **证据与触发**：Rust 先用可选 role_hint，其次用同 engine 的码缓存推导互补方向，否则默认 Receiver。Dart JoinSpec 和真实服务没有传 role。不同设备的接收方生成码后，加入方因此也声明 Receiver。TaskPairing 要等 redeem 完成 PairCommit、双侧落簿和 PairReady 后，才解 endpoint 并检查方向互补；随后返回 DirectionConflict。
- **影响**：正常“接收方生码、发送方输码”流程失败，并消耗 `max_redeems=1`，留下已提交信任关系和不一致的任务状态。同方向或 malformed/missing endpoint 的其他任务级失败也发生在不可逆提交之后，不能把返回错误理解为无副作用。
- **修复建议**：让加入方在 Commit 之前取得或协商发起方向并自动互补；owner 在 Prepared 前校验 joiner 的任务上下文。需要调整控制契约时先明确兼容扩展，保留旧流。不要用 UI 额外猜角色、或对已经 Commit 的事务回退计数来遮盖问题。
- **覆盖缺口**：FFI 跨引擎接收方码测试在 `task.rs:3663` 人工提供 `Some("sender")`，绕过了 Dart 真实调用；session 的 direction-conflict 测试只断言错误，没有核查码可继续使用或 peer book 未增加。
- **置信度**：高。

### P1-06：真实连接流不向后续订阅者回放快照，进入详情页会误判断开并失去发送入口

- **位置**：`flutter_app/lib/services/simple_transfer_service.dart:223`、`:226`、`:377`；`flutter_app/lib/ui/pages/home_page.dart:51`；`flutter_app/lib/ui/pages/connection_detail_page.dart:42`、`:173`、`:199`。
- **证据与触发**：真实服务用 broadcast controller 的 onListen 发送当前列表，但 onListen 只在监听数从零变为非零时调用。主页已有监听，用户在连接稳定后打开详情，新增监听得不到当前快照，`_connection` 保持 null；没有后续状态事件时一直显示“此连接已断开”。
- **影响**：在真实服务下，正常已配对连接无法可靠进入发送页面。不能依赖另一连接恰好产生事件来刷新当前详情。
- **修复建议**：实现逐订阅者的当前快照加后续事件，或在页面初始化时使用明确同步快照 API 并处理订阅交接竞态。将该语义纳入 UI service 契约。
- **覆盖缺口**：Fake 的 connections 使用每次订阅先 yield 当前列表的实现，反而具备真实服务缺少的语义；现有 connections widget 测试因此未覆盖真实缺陷。
- **置信度**：高。

## P2 发现（15 项）

### P2-01：FFI request_id 去重既非原子，也没有绑定操作和连接上下文

- **位置**：`crates/ak-ffi/src/task.rs:1437`、`:1459`、`:1478`、`:1504`、`:1531`、`:1616`、`:1654`；契约 `docs/contracts/ffi-v2.md:438`。
- **证据与触发**：replay_check 查询后即释放锁，launch 先注册/启动操作，最后才 record_request_id。两个线程可同时查到不存在并各启动一次。去重内容又只有 JSON，而 send 的 conn_id 是独立 C 参数，没有进入 canonical spec；不同操作共用同一 request_ids 表，也没有操作类型域。
- **影响**：同请求的并发重试可产生两个码/兑换/发送；先对连接 A 发送，再以相同 request_id 和 paths 对连接 B 发送，会错误返回 A 的 op，而非拒绝冲突。使用一个包含各操作所需字段的 JSON 还可跨操作误命中。发送入口先检查当前连接状态，也使断开后的原请求重放无法取得原答案。
- **修复建议**：以运行时、操作类型及完整语义参数（包括 conn_id）建立请求指纹；把查找、冲突判定、预留 op_id 和登记做成一次原子受理，然后启动副作用。重放已受理操作应先返回原记录，再决定是否允许新操作。
- **覆盖缺口**：当前 `task.rs:3080` 的测试仅串行 create 重放。建议覆盖并发受理、跨 conn、跨 API 及终态后重放。
- **置信度**：高。

### P2-02：任务栈的并发懒初始化会建立重复身份连接，并保留已被替换的客户端

- **位置**：`crates/ak-ffi/src/task.rs:945`、`:954`、`:1035`、`:1040`；`crates/ak-signal/src/server_v2/engine.rs:916`。
- **证据与触发**：ensure_stack 在检查空槽后放开锁，跨 await 完成 TLS 登录、orchestrator 和后台任务初始化，直到最后才决定谁写槽。两个首次 create/join 并发时都会登录同一 device_id；服务器明确让新连接关闭旧连接。第二个初始化发现已有槽后，只 abort 自己的三个后台任务并返回旧栈，没有恢复被替换的旧控制连接，也未显式 close 新 client。
- **影响**：合法并发启动就可能使最终保存的栈处于离线/被替换状态，后续请求持续失败；同时产生多余连接和后台资源。单独修复 P2-01 不能消除不同 request_id 的这条竞态。
- **修复建议**：对初始化使用共享进行中 Future、异步互斥或 OnceCell，使一个运行时只执行一次登录；失败后可明确重试，淘汰路径必须关闭实际客户端。不要把“最后存入一次”当作“只初始化一次”。
- **覆盖缺口**：建议并发两个不同首次操作，核查实际登录次数和最终持有连接的可用性。
- **置信度**：高，具体受影响操作取决于调度顺序。

### P2-03：已配对但空闲时断开没有跨设备通知，双端生命周期契约不成立

- **位置**：`crates/ak-ffi/src/task.rs:1726`、`:1757`、`:1767`、`:1784`、`:2345`；`crates/ak-session/src/pairing/task.rs:886`、`:910`；`crates/ak-session/src/pairing/orchestrator.rs:398`；契约 `docs/contracts/ffi-v2.md:441`。
- **证据与触发**：远端通知依赖正在传输的 QUIC 连接被 abort 后另一侧报告 PeerClosed；配对和探测完成、尚未发送文件时没有持久数据连接可承担通知。直接通知只通过同 engine 缓存。owner revoke 处理码及未提交兑换，不是给 Ready 对端发送 TaskClose；joiner 本地不持有 owner 的码，revoke 得到 unknown invite code 后被容忍为本地完成。
- **影响**：一侧点击断开返回成功，另一设备仍显示 connected，可能继续尝试已经不存在的接收端；双方空闲同时断开也只是各自结束，不能证明单侧通知语义。FFI 还丢弃异步 revoke 错误，未报告待同步状态。
- **修复建议**：实现经认证、绑定 task/pairing 的关闭通知及确认，明确空闲、传输中、离线和同时关闭的幂等语义。可使用必要控制消息，但不能由此引入文件中继；本地取消与对端确认应分别可观察。
- **覆盖缺口**：`task.rs:3646` 的跨 engine 测试先开始真实传输并接受 Offer，再断开；它无法覆盖“配对后不发送”的普通路径。同 engine 通知测试也不能证明跨设备行为。
- **置信度**：高。

### P2-04：生成但未加入的码无法取消，默认无限 TTL 会留下长期注册行与监听资源

- **位置**：`flutter_app/lib/ui/service/pairing_flow_controller.dart:135`；`flutter_app/packages/ak_ffi_dart/lib/src/task_ffi.dart:168`；`crates/ak-ffi/src/task.rs:1555`、`:1737`、`:1939`、`:1955`、`:485`；`crates/ak-signal/src/server_v2/store.rs:549`；`crates/ak-session/src/pairing/task.rs:784`、`:990`。
- **证据与触发**：生成操作返回 code_id，但在对端加入前没有 TaskConn；ak_task_disconnect 只查 conns。UI reset 只清页面字段，没有撤码。Dart 与 FFI 默认 ttl_secs=0，原样传给服务器，含义为长期有效，绕过了 TaskPairing 库的十分钟默认。未加入码的 receiver/echo 只能等 engine teardown；即使指定有限 TTL，会话任务表也没有到期清理。teardown 仅删本地缓存、关闭 client，未撤销服务器注册行。
- **影响**：用户取消/重新生成后，旧码在本次运行中仍可兑换，旧监听持续存在；应用退出后留下没有本地秘密可继续应答的长期 active 注册行，反复使用可耗尽 owner active-code 配额。UI 删除旧邀请码管理页后，也没有管理这些孤儿行的正常路径。
- **修复建议**：让创建中的任务也有可关闭标识，打通取消创建、未加入撤码、超时回收和应用关闭语义；关闭时清理 receiver/echo/secret/active task 并对服务器撤码结果记账。统一任务码与库默认 TTL 的产品约定，不能依赖当前不会过期的行“自行到期”。
- **覆盖缺口**：建议覆盖生成后取消、码过期无人加入、取消与 Commit 竞争、应用退出重启以及多轮创建后的配额回收。
- **置信度**：高。

### P2-05：接收器只处理一次连接，成功或拒绝后消失，但任务仍显示 connected

- **位置**：`crates/ak-ffi/src/task.rs:2024`、`:2091`、`:2093`、`:2106`；`crates/ak-engine/src/lib.rs:2793`、`:2796`；`crates/ak-ffi/src/task.rs:1635`。
- **证据与触发**：所谓 resident receiver 实际只调用一次 receive_with_addr_consent。成功只发 done，ConsentDeclined 分支直接结束，两种路径都不标记连接终态，也不撤销本地任务。原监听 socket 随函数返回消失，连接列表却继续存在，发送 API 仍接受新操作。
- **影响**：接受一次传输或拒绝一次 Offer 后，界面继续呈现可用连接，再次发送只能连接失败；用户也得不到应创建新任务码的可靠终态提示。
- **修复建议**：明确“一次任务”与“一个可多次 Offer 的连接”的区别。若设计为单传输任务，完成/拒绝后必须收敛双端终态及 UI；若保留连接供再次发文件，应复用同一认证监听/通道，不能以注释代替实现。
- **覆盖缺口**：现有端到端接受/拒绝测试各执行一次。应覆盖同一连接后续操作，并断言其按契约成功或明确终态拒绝。
- **置信度**：高。

### P2-06：保存文件夹在生码/加入时已被接收器捕获，后来修改设置不会影响待接收任务

- **位置**：`crates/ak-ffi/src/task.rs:2050`、`:2057`、`:1796`、`:1812`；`flutter_app/lib/ui/widgets/incoming_requests_host.dart:93`。
- **证据与触发**：receiver 启动时 clone save_dir 并构造 ReceiveSpec，之后一直持有原 out_dir；set_save_dir 仅修改 TaskRt 中的字段。先生成接收码/加入成为接收方，再修改保存文件夹，最后同意入站请求，就会落入旧目录；横幅展示的却是 service 当前新目录。
- **影响**：用户明确选择的保存位置与实际落盘位置不符，可能把文件保存在错误位置，且成功返回不提示差异。
- **修复建议**：在 Offer 同意/创建保存目标时解析有效目录，或把目录作为该接收任务不可变属性并在请求上展示真实目标；更改设置对未开始任务的生效时点必须一致。目录变更仍需复用既有安全目标和路径防护。
- **覆盖缺口**：现有测试在 receiver 创建之前设置目录，没有覆盖生码后或 pending consent 期间改目录。
- **置信度**：高。

### P2-07：接收同意用 Condvar 阻塞 Tokio worker，两个挂起请求会饿死整个引擎运行时

- **位置**：`crates/ak-engine/src/lib.rs:692`、`:3488`；`crates/ak-ffi/src/task.rs:586`、`:647`；`crates/ak-ffi/src/core.rs:919`、`:920`。
- **证据与触发**：异步 receive future 直接调用同步 consent Fn，FFI 回调在 Condvar 上等待至 30 秒；没有转到 blocking executor。AkEngine 固定两个 worker，两个接收任务同时等待用户就能占满它们。
- **影响**：其他传输、QUIC/TLS 驱动、registry、probe 和异步取消在等待窗口内无法正常调度，并可能发生连带超时。外部 FFI 线程作出决定仍能唤醒，所以这里是确定的运行时饥饿风险，不是无条件永久死锁。单纯 task.abort 也不能及时终止已经阻塞的 gate。
- **修复建议**：将 consent 改为异步 Future/oneshot，把同意、30 秒到期和任务断开放入统一取消机制；临时保留同步接口时须使用有界 blocking executor。断开应唤醒本连接 pending 请求，不应仅等待窗口自行结束。
- **覆盖缺口**：需覆盖两个 pending consent 与第三个正常传输/控制请求并存，以及 pending 时断开。本轮没有执行并发实验。
- **置信度**：高。

### P2-08：协调连接断开被错误映射为全部任务终态，和仍健康的数据直连状态分裂

- **位置**：`crates/ak-session/src/pairing/task.rs:960`、`:967`、`:975`；`crates/ak-ffi/src/task.rs:1065`、`:1083`、`:1635`；`docs/adr/001-direct-only.zh-CN.md` 的 INV-007。
- **证据与触发**：network watcher 收到 registry Disconnected 后，对所有 task 调用 report_connection_lost，无论对应直连是否仍健康。FFI pump 据此标记连接 disconnected，但没有同步取消对应数据任务。
- **影响**：服务器维护或控制链路短断会导致 UI 删除/判死连接、后续 send 被拒绝；正在进行的文件流却可能继续。不能把这一源码结论写成“已有传输必然被中断”，但它已经破坏任务状态正确性，并把直连生命周期不必要地依赖于服务器。
- **修复建议**：分离协调可用性与已授权数据通道状态；健康且路径未变化的直连继续有效，尚未完成协调的任务进入等待/重试。真正的数据连接关闭再驱动任务终态。
- **覆盖缺口**：需要 task FFI 链路的 control-only loss 场景；旧引擎/CLI 的服务器关停测试不能代替新 watcher 的行为验证。
- **置信度**：高。

### P2-09：连接状态归并忽略探测失败，并可能被迟到的 join 结果退回 probing

- **位置**：`flutter_app/lib/services/simple_transfer_service.dart:343`、`:353`、`:562`、`:568`；`crates/ak-ffi/src/task.rs:1144`、`:1148`、`:2246`、`:2254`、`:2259`、`:2263`。
- **证据与触发**：Rust 在 probe Success/AllFailed 后都置 Connected；Dart 收到 TaskProbeResultEvent 也不读 reachable，false/null 同样视为成功。另一条时序中，原生先发 connected/probe_result 再完成 join op；Dart 事件泵已经处理完成后，join Future 又无条件把该行重建为 probing、全局 phase 设为 paired。结果到达前已处理的 disconnected 也可能被覆盖。
- **影响**：既可能“不可达却显示成功”，也可能“已完成探测却永久测试中”，甚至在 UI 层复活已断开的行。这些不等于绕过 TLS 文件身份验证，但会误导核心交互。
- **修复建议**：统一结果和事件的状态归并，保留单调终态与任务版本；明确 paired、探测中、可达、不可达和 disconnected 的区别。探测真值才用于可达提示，不能只凭事件类型认定成功。
- **覆盖缺口**：覆盖事件先于 op 结果、结果先于事件、探测 false/null，以及结果返回前断开。当前 Fake 的稳定顺序未覆盖真实泵时序。
- **置信度**：高；竞态是否出现取决于事件/结果调度。

### P2-10：固定 120/180 秒整体传输超时只停止 UI 等待，原生任务仍可能继续写入

- **位置**：`flutter_app/lib/services/simple_transfer_service.dart:404`、`:407`、`:411`；`flutter_app/lib/services/ffi_simple_transfer_service.dart:184`、`:197`、`:206`；`flutter_app/packages/ak_ffi_dart/lib/src/task_ffi.dart:725`、`:743`。
- **证据与触发**：发送等待默认整体 120 秒，接收同意后的完成等待整体 180 秒，UI adapter 不按文件量或进度调整。TaskOpHandle.wait 超时仅抛异常，没有取消 native op 或保存可恢复的操作跟踪；接受侧只等 done，没有及时处理失败/断开。
- **影响**：大文件、慢链路或源扫描仍在正常推进时，界面会报失败，实际发送/落盘继续；用户重试可能重复发起任务。这与 Rust 已正确拆出的 10 秒“连接阶段预算”是不同问题。
- **修复建议**：以真实终态和无进展时限跟踪任务，不用固定总时长判失败；保留 op/request id 供继续查询。显式取消应贯通原生取消并等待终态，接收等待也应消费失败/断开。
- **覆盖缺口**：现有 Fake 传输快速完成。建议覆盖超过该时间且持续有进展的操作，以及超时后恢复查询/显式取消。
- **置信度**：高。

### P2-11：30 秒自动拒绝没有关闭横幅，过期后点拒绝会产生未处理异步异常

- **位置**：`flutter_app/lib/ui/widgets/incoming_requests_host.dart:40`、`:55`、`:59`；`flutter_app/lib/services/ffi_simple_transfer_service.dart:184`、`:187`；`crates/ak-ffi/src/task.rs:640`。
- **证据与触发**：UI 只将 incoming 请求加入列表，没有到期或请求关闭事件。Rust 超时移除 pending 后，横幅依旧可接受/拒绝；点拒绝时 UI 先移除，再 await service.consent，且该分支在 try/catch 之外。adapter 对未知/过期请求明确抛 StateError。
- **影响**：用户面对已经失效的操作，拒绝路径产生未处理异步错误。未发现这会导致自动接受文件；问题是终态同步及错误处理缺失。
- **修复建议**：以明确请求终态事件关闭/更新横幅；UI 对已失效拒绝幂等收尾，并统一捕获接受和拒绝错误。不要仅靠一个无法与后台时钟同步的界面倒计时保证正确性。
- **覆盖缺口**：Fake 的即时拒绝总是成功。需要静默到期后再接受、再拒绝，以及并发多请求的场景。
- **置信度**：高。

### P2-12：新增 UDP echo 的响应仍是合法请求，可在条件满足时形成持续互相应答

- **位置**：`crates/ak-ffi/src/task.rs:1251`、`:1263`、`:1266`、`:1270`；`crates/ak-net/src/lib.rs:1087`。
- **证据与触发**：常驻 responder 绑定 wildcard，对任意来源的 `AKPUNCH1` 加 16B 报文回复同 tag 加 16 个零；is_probe_packet 只看 24B 长度和 tag，因此回复也会被另一 responder 当请求。两个端口相互可达，且有人能够向 A 注入来源伪造为 B 的合规 UDP 包时，可以触发 A→B→A 连续应答。常规生码/加入会自动创建 responder。
- **影响与前提**：攻击者不需要任务码，但需要端口可达、获知端口和源地址伪造条件；不能泛化为任意互联网攻击者可攻击任意 NAT 后设备。单步是 24B→24B 的 **1:1 反射**，没有包体或指数放大；问题是一包种子可产生没有协议终止条件的持续流量与 CPU 消耗。丢包或 socket 关闭可以终止某条循环，本轮未动态复现。
- **修复建议**：区分请求/响应，响应绝不再次应答；加短寿命会话/nonce 绑定、每端点和全局速率/总量上限。对自报候选限制无效地址及探测扇出，正常局域网支持不应被简单的“禁止私网地址”替代。
- **覆盖缺口**：已有主动 probe 有 deadline 且只接收记录，不会自行互回；应专门覆盖新 responder 的响应互投、未经授权来源和预算耗尽。
- **置信度**：高（代码与条件路径）；现实可利用性取决于上述网络前提，因此列 P2。

### P2-13：SQLite v1→v2 迁移不原子，中断后会留下无法自动重启的半迁移库

- **位置**：`crates/ak-signal/src/server_v2/store.rs:403`、`:409`。
- **证据与触发**：四个 ALTER TABLE 在普通 Connection 的 execute_batch 中执行，没有显式事务；user_version 在下一条 pragma_update 才更新。某条 ALTER 已提交后退出、断电或后续写入失败，会留下列已存在但版本仍为 1 的状态；下次从第一条 ADD COLUMN 重做即 duplicate column。
- **影响**：一次升级中断变成注册服务持续启动失败，影响全部配对/协调业务。synchronous=FULL 只保证单次提交的持久性，不能把这些独立语句变成整体事务。
- **修复建议**：用一个明确事务同时覆盖所有 DDL 和版本更新，失败整体回滚；对已经产生的半迁移布局提供受控识别和恢复，不删除库、tombstone 或唯一性记录。
- **覆盖缺口**：`store.rs:2392` 的迁移测试只覆盖正常完成，应覆盖各持久化边界的中断恢复。
- **置信度**：高。

### P2-14：旧 claim journal 未转换，请求体编码变化破坏升级前后的幂等重试

- **位置**：`crates/ak-signal/src/server_v2/store.rs:469`、`:473`、`:1427`、`:1430`、`:1431`；迁移处 `:403`；基线为同文件在 `5a5dac1` 的 claim_body。
- **证据与触发**：旧 claim 的 canonical body 为 lookup_digest[32]+ttl_secs[4]，共 36B；新版即便无扩展也无条件追加 max_redeems[4] 与 endpoint presence[1]，变成 41B。迁移保留旧 request_journal 字节，claim 又按整段字节比较。升级前已提交但响应丢失的旧客户端，升级后重发相同请求会收到 REQUEST_CONFLICT。
- **影响**：破坏持久幂等恢复和旧 invite 流的升级兼容。升级服务器后旧客户端正常创建一个新码成功，不能证明这个边界兼容。
- **修复建议**：对默认扩展保留旧 canonical 编码，或在迁移事务内只对旧 claim 记录做明确版本转换；避免误改 revoke 等请求。升级前后的相同请求必须返回原答案，真正改变扩展才拒绝冲突。
- **覆盖缺口**：迁移 fixture 建了 journal 表却未插入旧幂等记录。应以旧版本持久化记录覆盖升级重放及不同内容冲突。
- **置信度**：高。

### P2-15：endpoint 缓存只有单项预算，没有总量与终态回收，失败配对会长期积存

- **位置**：`crates/ak-session/src/pairing/client.rs:152`、`:197`、`:205`、`:228`、`:248`、`:303`、`:510`。
- **证据与触发**：每个带 endpoint 的 InviteRedeemOffered/PairCommit 都按 pairing_id 插入 HashMap，插入发生在是否有消费者/事务的判断之前；没有容量或 TTL。PairError、PairReady 和 dropped unmatched frame 不清理，只能由成功消费路径 take 或整个 registry 断开清空。owner 拒绝证明、超时或旧 owner loop 没有任务 endpoint 消费者时，条目不会正常回收。
- **影响**：即使每个 blob≤512B，长时间在线期间失败或陌生事务仍能累计占用内存；服务器瞬时 pending 上限和帧限频不能约束客户端历史累计量。不据此宣称可立即 OOM，耗尽时间取决于流量、运行时长与服务端约束。
- **修复建议**：将 blob 存在受限在途配对对象中，拒绝为未知事务无限积存，在失败、结束、超时、取消时清理；容量满应明确拒绝新工作，而非淘汰仍有安全语义的活动记录。
- **覆盖缺口**：应覆盖连续失败 owner flow、陌生 PairCommit、取消和无消费者情形，并核查总量上限。
- **置信度**：高（缓存生命周期）；资源影响大小依运行条件而变。

## 重点核对与未另计发现的结论

### 单次兑换、撤销及兼容

撤销与最终 Commit 都进入 SQLite 写事务，Commit 在 `store.rs:1010` 复查 active/expiry；没有确认“撤销先提交、后来 Commit 仍被接受”的新绕过。查码到 begin 之间存在旧快照窗口，可以留下最终被拒绝的 pending，不能据此宣称撤销无效。单次额度问题是 P1-03 的独立缺口。

同一次运行中的 redeem 重放优先命中内存 journal；服务端重启后该 journal 不保留，是波次前已经存在的恢复边界。新的单次码语义下，重启后同 request_id 可能遇到 REDEEM_EXHAUSTED，而非原 Pending，应补恢复覆盖；本报告没有把这个既存机制伪称为本波新引入的独立缺陷。claim 的跨版本字节编码回归则是本波可明确定位的新问题，已列 P2-14。

无扩展时，四个消息不写额外字节，新增字段由帧层承载而非改变旧 CtrlMessage 布局；默认 max_redeems=0 保持旧流。解码只允许指定 tag/flags，拒绝未知标记和尾随数据。源码支持“旧客户端正常旧请求在新服务器上的线格式兼容”，但不支持把它扩大为所有升级恢复情况均兼容，也不自动承诺新扩展客户端可与旧服务器互通。

### endpoint 的信任与预算

`crates/ak-signal/src/protocol_v2/message.rs:1032`、`:1307` 在编码、分配 blob 前解码均实施 1..=512B；`frame.rs:198` 与 `server_v2/framing.rs:36` 保持帧总预算。会话层 `crates/ak-session/src/pairing/task.rs:208`、`:221` 还检查解释预算和候选数上限 16。没有发现普通输入突破这些单项限制的路径。

endpoint 不受配对 transcript 签名覆盖，是 `protocol-v2.md:233` 明示的设计；不能因为它未签名就直接推断配对公钥可被替换。真实发送仍在 `ak-ffi/src/task.rs:2293` 取已验证 peer pin，并在 `:2374` 构造 SendSpec，走真实 QUIC 身份和路径检查。

但是，自报地址会触发真实出站行为：`ak-session/src/pairing/task.rs:537` 只排除 unspecified，loopback、私网、多播及第三方地址并未全面限定；主动探测有单轮 1.5 秒/150ms 和候选数边界，但自报地址不是“属于已配对设备”的证明。`ak-net/src/lib.rs:1174` 只认 probe 形状，不验证 nonce；`task.rs:582` 还允许非候选来源作为成功依据。FFI 为 probe 单独绑定 echo 端口，因此 echo 成功也不证明文件 QUIC 端口可用，更不证明跨 NAT 数据通道已建立。应将这些值严格作为连通提示，并为自动探测设置整体预算；不得借 reachable 或 Observation 布尔值放行文件。与此相关的确定资源问题分别列为 P2-12、P2-15，接收身份问题列为 P1-04。

没有发现本波在正常代码路径将文件 payload 送往 registry、TURN 或 HTTP 文件上传；仍未进行抓包或真实路径实验，不能据此声明已证明所有部署环境不存在代理/中继。

### 接收同意、文件落盘及内存边界

`ak-engine/src/lib.rs:3481` 的 consent 位于 Accept、Manifest、sink 和检查点记录写入之前；false 路径先尝试发 DECLINED Error，finish 发送侧，再有界 drain 后退出。该“DECLINED”是复用既有 LocalPermissionDenied 错误码加固定消息，不能误述为新增 wire tag。丢失链路时对端收到错误帧并非无条件保证，但不存在靠拒绝后继续落盘完成任务的分支。

“零落盘”应精确解释为零用户文件、分片和检查点记录写入：保存目录可能在 task_start/set_save_dir 时创建，`receive_job` 在 consent 前也会 `FileCheckpointStore::open` 创建检查点目录。不能把当前实现说成拒绝前绝无文件系统写操作。路径处理继续调用既有 LocalSaveTarget/引擎保存流程，本波未用字符串替换自行实现文件接收；没有从这次 diff 确认新的路径越界或符号链接绕过，既有平台防护与限制不能因此当作重新验证通过。

常规单个 consent 请求的决定在 pending 锁内串行化，已决定/未知 id 返回 -4，到期默认 false，没有自动接受路径。问题在运行时阻塞、关闭唤醒、请求终态和 UI 归并，见相应发现。

FFI 出参在副作用前检查必要输出指针，字符串容量不足不消费结果，异步主体持有 Arc 状态而非 AkEngine 借用，未确认新增野指针路径。`ak_task_*` 的状态和幂等仍有 P2-01/P2-02 等问题，内存 ABI 形状正确不代表业务生命周期正确。

### Dart dispose、旧页删除与验证覆盖

`flutter_app/lib/services/simple_transfer_service.dart:285` 先置 `_disposed=true`，再停 Timer、关闭流；create/join/send 的 wait 传入 ensureLive，`task_ffi.dart:734` 每轮在 native poll 前执行检查。wave-26 P1-05 的主要防悬空调用模式得以保留。本轮没有确认这条服务链新的 free 后轮询；真正 engine owner 的生产释放接线仍须随 P1-01 完成，不能把未接线状态当作所有权已验收。

没有发现已删除旧页面/组件的可执行 import 残留。存在指向已删除集成测试的过期文档/注释，例如 `flutter_app/pubspec.yaml:60`。本波删除 `flutter_app/integration_test/android_real_test.dart`、`app_e2e_test.dart`、`invite_e2e_test.dart`，新 UI 测试主要验证 Fake。已有测试数量不反映这一覆盖形态的变化。

Android 的系统选择器已接普通路径返回，但公共目录 URI 授权与 Rust 普通路径访问仍不是同一个能力；SAF 实机接线在项目历史中本就未完全验收。本轮未执行平台验证，不把这些既存边界新增计为确定平台漏洞，也不将“使用系统选择器”表述成公共目录收发已经验证。

## 契约与实现对照

| 契约 | 本轮静态结论 | 对应处置 |
|---|---|---|
| protocol-v2 §10a | 无扩展旧字节和 512B 编解码预算吻合；endpoint 明确为提示 | 保留；配套自动探测/缓存总预算，见 P2-12/P2-15 |
| protocol-v2 单次兑换语义 | 计数状态集合正确，但提交处不复核 | P1-03 |
| pairing-flow §4a | 一次提交耗额度的口径明确；并发恰好双方和失败任务语义未兑现 | P1-03/P1-05 |
| pairing-flow §6a | endpoint 经 Offered/Commit 交付已实现；owner 方向在不可逆提交前不可供当前默认 join 使用 | P1-05 |
| pairing-flow §12a / storage-schema v2 | 四个新列与文字增补存在；迁移非原子、旧 journal 编码未转换 | P2-13/P2-14 |
| ffi-v2 §8 导出表 | 八导出形状一致，共用结果/事件通道的代码存在 | 未发现新 ABI 形状问题 |
| ffi-v2 §8.1/§8.2 码与方向 | 示例 AKT1、UI 八位、实现 AKI1 三种约定并存；自动互补方向没有跨设备闭环 | P1-02/P1-05，统一格式与方向协商 |
| ffi-v2 §8.4 幂等/断开 | 并发受理及参数上下文未完整去重；空闲远端通知不成立 | P2-01/P2-03 |
| ffi-v2 §8.4 consent | 拒绝时序和 30 秒默认拒绝有实现；调度、终态传播不完整 | P2-07/P2-11 |
| ffi-v2 §8.5 诚实边界 | `:446` 仍称注册协调/探测未接、跨 engine 必失败、存在 B-PENDING/ignore 占位；实际 Rust 已接真实路径且相关测试不再 ignore | 更新为最终实现事实，删除过期并行占位说明 |

还有几处维护文案需要与最终状态同步，但未重复计入发现数：`ffi_simple_transfer_service.dart:34` 仍称 AKT1 12hex、48bit；`:40` 仍称 probe 未接；`task.rs:94` 等开头注释声称顺序常驻接收，`:2024` 实际明确单次接收。`pairing-flow.md:12a` 的“storage DDL 待集成者同步”与 storage-schema 已有文字增补也应统一到明确版本。README/HANDOFF/ROADMAP 保留大量历史口径，应把 wave28 当前实现与旧证据入口标清。

## 架构与可维护性评价

将 UI 收敛为连接与设置、把接收同意放在 Offer 与 Accept 之间、以有界帧扩展保持旧线格式、把 C 指针适配与安全 Rust 逻辑分开，这些方向合理。实际问题集中在模块交界及所有权收口：UI 与服务分别冻结了不同码格式，Fake 与真实实现的流语义不同，临时注释和替换点没有在集成结束时消除。

任务生命周期至少散布在 TaskPairing.tasks、FFI conns/codes/spawned/pending、Dart connections/phase 和 UI flow 中；多处独立写终态，缺少同一个任务状态来源及版本。这能解释 pending 无法关闭、完成后仍 connected、控制断线假终态、结果覆盖事件等多个缺陷。建议由 session/任务运行时统一拥有创建、配对、探测、Offer、传输、关闭的状态与资源，FFI 和 Dart 负责映射，避免再次各自重建业务状态。

`task.rs` 是约 3740 行的新增聚合文件，同时处理注册协调、连接状态、UDP echo、接收器、幂等、事件和 ABI 场景测试；两个同名 SimpleTransferService 加 adapter 的分层可以保留，但必须共享唯一的数据和生命周期契约。维护改进应优先抽出状态归并、资源关闭和身份准入，而不只是移动文件。探测、控制协调和文件传输也应有不同的状态和证据口径，避免一个 reachable 或 connected 名称承载多种含义。

## 后续验证建议与证据边界

以下仅是修复后的建议门槛，**本轮均未执行**：

| 验证组 | 应覆盖的关键场景 |
|---|---|
| 实际 GUI 接线 | 真实 main 启动；双方各生一次码；真实码复制/输入；稳定连接进入详情；选择文件、同意、字节一致 |
| 单次额度与恢复 | 两个 pending 竞争 Commit；cap>1；同 request_id 重放；撤销交错；旧 v1 journal 升级重放；迁移中断 |
| 身份与 endpoint | 第三身份抢先拨号不触发合法任务提示且不消耗 listener；512B/513B；未知候选/伪 probe 不变为身份授权；echo 响应不再次触发应答 |
| 生命周期 | 生码未加入后取消/退出/到期；空闲单侧和双侧断开；pending Offer 断开；完成/拒绝后下一操作；registry 单独断开 |
| 并发与时序 | 同 request_id 并发；不同首次 op 并发初始化；两个 pending consent 不阻塞其他工作；join 结果/事件两种顺序；晚订阅快照 |
| 存储与长传输 | 生码后改保存目录；超过 120/180 秒仍有进展；自动拒绝后横幅终态；Android 系统目录授权与真实 Rust 落盘 |

既有 1085/0、128/128、86/86 和跨设备 6/6 记录应继续保留其原始命令、二进制/提交版本及覆盖范围，不能因为本次发现而否定已经实际执行的结果；同样不能用这些汇总计数覆盖没有触发的代码分支。本轮结论来自源码、契约与历史材料，未新增任何测试通过、动态漏洞复现、部署验证或抓包级无中继声明。
