# AK-sending 第二十五波质量与正确性终审

## 执行摘要

**结论：当前 HEAD 不宜按“质量终审通过、全部发现关闭”收官。** 本次确认 **20 项发现：P0 阻断 1 项、P1 重要 6 项、P2 建议 13 项**。发现均来自静态代码与文档证据，没有执行复现；逐项置信度和适用条件见正文。P0 是当前真实 GUI 可达的 FFI 参数错配：切换调度策略后读取快照时，Rust 会把 Dart 的短查询字符串当作输出缓冲区，造成原生堆越界写。应先统一 ABI，再继续验收该入口。

第二十五波的 64-hex registry_id 修改及默认值修正，在已检查的 Dart/Flutter 调用方中一致，没有发现残留的 32-hex 实参。多地址去重与稳定排序的基本实现正确，但 3 秒超时覆盖了文件扫描，正常大文件可能在真正开始握手时就耗尽连接预算；IPv6 输入与实际 IPv4 bind 也未对齐。第二十四波 daemon 确实把真实接收引擎放进了 SessionRuntime，但每轮只有一个任务、所有任务使用同一匿名码组，尚不能证明或实现真实邀请码的两维并发。真实 executor 关闭、配对启动/停止以及 Dart dispose 还有生命周期缺口。

**评审对象与方式：** HEAD 为 `9caea1197389b173c69ef4d70d864c4aa8aac731`，主要增量为 `acbaf27..HEAD` 的 `ac6b23b`、`3bc75e0`、`210d4b0`、`9caea11`；同时追踪当前 FFI、引擎、网络、存储及冻结契约，复核前次报告中的未闭环事项。按模块并行只读审查，主审复核关键调用链、分级和去重。初始工作树干净；本任务只新增本报告。

**验证边界：** 按本次明确要求，未执行 cargo、dart、flutter、Python、构建、测试、脚本自测、语法检查或仓库检查命令，也未连接设备或注册服务。用户提供的 Rust/Dart/Flutter 全绿结果及跨设备 6/6 记录均作为历史证据，不能视作本次复验，也不能外推到未覆盖的 ABI、取消时序和多任务场景。报告所列后续验证均为建议，未执行。没有把 Observation 的模型值当作密码学证明，没有得出抓包级无中继结论。

P0/P1/P2 分别表示阻断当前验收的重要正确性问题、应优先修复的功能或生命周期问题、局部行为/维护/证据问题。既存但此前漏检的问题仍计入本次发现，并标注归属；前次已经披露且仍开放的两项 P2 另列复核，不重复计数。

## P0 阻断（1 项）

### P0-01：Dart 调度快照与 Rust ABI 参数错位，真实 GUI 会触发原生堆越界写

**归属：既存跨层缺陷，本轮终审确认仍存在。置信度：高。**

证据：

- `flutter_app/packages/ak_ffi_dart/lib/src/sched_ffi.dart:223` 定义四参数 `_QueryBufC(engine, query, buf, cap)`；`:256` 用它查找 `ak_sched_snapshot`；`:329`、`:333` 按这个顺序实际调用。
- `crates/ak-ffi/src/shim.rs:1350` 的真实导出只有三个参数 `(engine, buf, cap)`；`:1358`、`:1359` 取得快照后调用 `write_cstr`，`:86` 按 JSON 长度执行 `copy_nonoverlapping`。
- 当前 GUI 调用链为 `flutter_app/lib/ui/pages/invite_page.dart:420` → `flutter_app/lib/services/ffi_transfer_service.dart:369` → `flutter_app/lib/services/invite_service.dart:509`、`:513`，应用策略后立即调用 `sched.snapshot()`。

触发与影响：配对/调度 runtime 已就绪时，在真实 GUI 切换同码或跨码策略。默认查询为 `{}`，原生 UTF-8 分配仅 3 字节；Rust 将这个地址解释为输出缓冲区，将真正输出缓冲区的地址解释为容量。通常容量检查会通过，随后把数百字节快照写进短字符串分配，产生未定义行为、堆损坏或崩溃。此结论依据实际调用约定，不仅是第三方按文档调用的潜在风险。

修复建议：统一冻结 C 签名、Rust 导出、Dart typedef 和所有调用点；修复完成前停用该真实入口。`docs/contracts/ffi-v2.md:117` 实际还规定五参数版本，含 `runtime_id`，必须明确最终唯一契约，不能只把某一侧临时改到“能调用”。同文件规定的 enqueue/set_policy/cancel/poll_event 也与 `shim.rs:1250`、`:1292`、`:1320`、`:1386` 的现有参数不一致，应一并清理，避免继续以符号齐全代替 ABI 一致。

覆盖缺口：`flutter_app/packages/ak_ffi_dart/test/pairing_ffi_test.dart:9` 明确不使用 DLL，`:725` 的快照测试走 Fake；`flutter_app/test/invite_service_test.dart:222` 也走 Fake。Android 集成测试 `flutter_app/integration_test/android_real_test.dart:469` 的邀请码场景覆盖创建/撤销，没有覆盖策略快照。后续必须通过真实 DLL/so 验证策略设置和快照读写，并检查原生内存边界；`scripts/check_contracts.py:263` 只比较导出名称，无法发现本问题。

## P1 重要（6 项）

### P1-01：3 秒“连接预算”包含全量源扫描，正常大文件可能始终无法开始传输

**归属：第二十五波回归。置信度：高。**

证据：`crates/ak-cli/src/main.rs:2593` 创建整个 `ak_engine::send` future，`:2595` 用 3 秒 timeout 包住它；只有已经收到 Connected 才在 `:2598` 继续等待。`crates/ak-engine/src/lib.rs:2100` 在连接之前调用同步 `prepare_send`；`:2071` 打开全部源，`:2084` 保存检查点。`crates/ak-storage/src/lib.rs:370`、`:383` 的扫描循环读取整个文件并计算完整及分块摘要，之后才会到 engine `:2103` 的握手。

触发与影响：大文件、多文件目录或慢存储使准备阶段超过 3 秒，并在随后首次握手等待处返回 Pending。外层连接期限此时已经过期，CLI 报“no QUIC handshake”并取消尝试，即使接收方正常可达。同步扫描本身无法被该 timeout 及时抢占，所以实际耗时还可能明显超过 3 秒。`:2770` 每个地址重新构建并执行 SendSpec，会重复全量扫描；单地址发送也受到新预算影响，增大 `--timeout` 无法消除这层 3 秒限制。

修复建议：将准备与连接分开，对真正的 bind/connect 阶段单独计时；或向引擎传入独立的 connect_timeout。准备结果应在候选尝试间安全复用，传输仍使用原 idle budget。不要依赖整个发送 future 的进度回调来推断期限的起点。

覆盖缺口：`crates/ak-cli/src/main.rs:4549` 的真实回退测试使用小数据，验证黑洞地址超时和后续成功，没有覆盖准备耗时超过 3 秒。后续覆盖慢扫描、单地址大文件、首候选失败后不重复扫描，以及已连接传输超过 3 秒的正常情况。

### P1-02：daemon 的同码/跨码开关未控制真实邀请码并发，收官状态过早关闭

**归属：第二十四波真实接线不完整，第二十五波关闭状态仍不成立。置信度：高。**

证据：`crates/ak-cli/src/daemon.rs:386`、`:391` 为每次接收固定使用全零 code_id；`:397` 使用 `PairingPlan::Local`。`:680`、`:696` 的循环只提交一个任务，等 `:730` 的 watcher 完成后才在 `:780` 进入下一轮。`crates/ak-session/src/runtime/actor.rs:674` 的 Local 路径跳过真实会话，`:719` 到 `:746` 仅报告 AwaitingAdmission 后交给 executor；`crates/ak-session/src/runtime/real_executor.rs:258` 调用 legacy `receive_with_addr`，没有 on-channel TaskOpen/TaskAdmission。

触发与影响：用 `daemon --same-code parallel --cross-code parallel` 接收两个已配对发送端的并行任务，同时最多服务一个任务，其他并发发送可能等待或失败；真实 code_id 从未参与分组，不能区分同码和跨码。`docs/contracts/scheduler.md:42` 的真实分组、四模式矩阵与双端策略交集未落到该生产路径。`docs/HANDOFF.zh-CN.md:49` 宣称 P1-4 关闭，超出了“真实文件经过本地 scheduler”的实际实现。这里没有断言既有 TLS 身份授权被绕过。

修复建议：常驻绑定后，从认证通道、Ready relation 和 TaskOpen 得到真实组键，为独立传输取得双端许可，再进入 on-channel 引擎；显式手工指纹兼容路径单独保留。真实四模式行为完成前，将该里程碑恢复为未完成，并准确描述目前只接入了本地串行接收任务。

覆盖缺口：`crates/ak-cli/src/daemon.rs:1381` 的成功测试只有一次 legacy 传输；`crates/ak-itests/tests/dual_scheduler_modes.rs` 的模式测试使用 Fake executor。后续必须覆盖真实不同码、同码、双端不同策略和重叠传输区间。

### P1-03：真实引擎任务脱离 runtime 关闭生命周期，Drain 超时后仍可能接收和写文件

**归属：第二十四波接入真实 executor 后暴露。置信度：高。**

证据：`crates/ak-session/src/runtime/real_executor.rs:256` spawn 真实引擎，`:320` 仅保存 AbortHandle，未保留 JoinHandle；`crates/ak-session/src/runtime/actor.rs:746` 的 driver 在提交 executor 后即结束。关闭期限在 actor `:440` 到期后进入 `:624` 的 finalize_shutdown，其中 `:633` 只更新 core 状态，没有取消真实 executor；`:647` 只 abort/join driver，真实引擎不在集合内。

触发与影响：真实接收任务已经绑定端口、正在等待发送方，此时调用 shutdown(Drain)，等待关闭期限到达。在宿主进程与 Tokio runtime 继续存活的情况下，actor 可以发送 ShutdownComplete、把任务标成 Cancelled，真实 accept 仍持有 UDP 端口；活动传输也可能继续写入。这违反 `docs/contracts/scheduler.md:291` 的“等待所有子任务退出，再释放端口”。Cancel 路径 `real_executor.rs:343` 也只是 abort 后立即合成取消结果，没有等待真实退出；当 engine 已在 `crates/ak-engine/src/lib.rs:3055` 保存 completed、仍在 `:3069` 等待 FinalAck 尾部关闭时，还可能把已持久完成的任务报告为取消。

修复建议：让 executor 拥有可取消、可等待的真实工作句柄；Drain 超时转取消时必须逐个停止并 join，确认持久终态后再释放许可和报告关闭。取消与最终提交竞争时以已持久状态为准，不能单凭 abort 已提交就报告 Cancelled。

覆盖缺口：`crates/ak-itests/tests/cancel_and_shutdown_observable.rs:351` 的 Drain 场景使用 FakeClock/FakeTransport/FakeExecutor；真实 executor 测试仅覆盖成功和错误分类。后续验证真实 accept/传输中的 Drain 超时、取消后端口释放，以及提交完成与取消同时发生的结果。

### P1-04：配对服务在连接过程中停止，后台连接仍能在 stop 成功后重新上线

**归属：既存 FFI 生命周期缺陷，当前仍在。置信度：高。**

证据：`crates/ak-ffi/src/pairing.rs:410` 启动后台 connect，未登记其 JoinHandle。连接成功后 `:434` 启 owner loop、`:440` 安装 live client、`:446` 无条件写 Ready 并发送 runtime_ready。`crates/ak-ffi/src/sched_ext.rs:233` 先 teardown，`:235` 写 stopped，`:243` 登记已成功的 stop 结果；`pairing.rs:155` 的 teardown 只能清理已经登记的操作、owner loop 和 live client。

触发与影响：ak_pairing_start 后，TLS/注册握手尚未结束即调用 ak_pairing_stop_start；stop 成功返回，随后 connect 成功，runtime 又上线并可能自动应答兑换。停止操作的完成结果与后续实际行为不一致。teardown 的 `pairing.rs:164` 还清空旧 ops，与 `sched_ext.rs:223` 声称旧操作结果仍可读相反。

修复建议：把 connect 纳入 runtime 所有权，stop 先原子进入终态，再取消/等待所有启动子任务；安装 live client 前在同一同步规则下校验仍处于 Starting。保留需要补取的操作终态，或明确冻结停止后的查询语义。

覆盖缺口：`crates/ak-ffi/tests/ffi_pairing.rs:1459` 的生命周期测试从已经 Ready 的 paired_fixture 开始。后续覆盖 Starting→stop→延迟连接完成，以及 stop 后旧操作结果读取。

### P1-05：Flutter service 释放后，未完成的异步轮询仍可能调用已释放的 native engine

**归属：既存跨语言生命周期缺陷，当前仍在。置信度：高。**

证据：`flutter_app/lib/services/invite_service.dart:570` 的 dispose 只设置标志、取消周期 Timer、关闭事件流；`:391` 的 _awaitReady 在 `:401` 的 delay 后继续 poll，没有检查已释放状态。`flutter_app/packages/ak_ffi_dart/lib/src/pairing_ffi.dart:786` 的 PairingOpHandle.wait 也在 `:805` 等待后继续 native poll。`flutter_app/lib/services/ffi_transfer_service.dart:800` 释放 invite 后，`:805` 随即释放 facade/native engine；绑定 `pairing_ffi.dart:504` 持有借用的裸 engine 指针，Rust `crates/ak-ffi/src/shim.rs:213` 的 free 会真正释放它。

触发与影响：创建/列表/兑换操作正在等待 Ready 或结果时，应用或测试释放整个 service。下一个 Future.delayed 唤醒后，轮询使用悬空指针，可能崩溃或破坏内存。取消 Timer 不会取消已经启动的 Future；方法入口的一次 disposed 检查也不能保护 await 之后。

修复建议：给启动与操作 waiter 统一的取消及完成生命周期；在释放 native owner 前结束全部 waiter。绑定每次应通过受控 owner 获取仍有效的句柄，并在已关闭后拒绝调用。同步 dispose 若无法等待，需先可靠失效所有调用通道，再由明确的异步关闭流程完成释放。

覆盖缺口：现有服务测试主要在操作完成后 teardown。后续覆盖 pending readiness、pending op、并发启动等待者期间的 dispose，并验证 dispose 后不再发生 native 调用。

### P1-06：Dart 调度入队规格缺少 Rust 必填 peer，正常公共 API 无法提交任务

**归属：既存 JSON 契约缺口，与 64-hex 修正无关。置信度：高。**

证据：`flutter_app/packages/ak_ffi_dart/lib/src/sched_ffi.dart:109` 的 AkSchedEnqueueSpec 没有 peer 属性，`:161` 的 JSON 也不输出 peer；`crates/ak-ffi/src/pairing.rs:1368` 却强制读取该字段，`:1370` 再解析 SocketAddr。`docs/contracts/ffi-v2.md` 的冻结 enqueue 示例同样未提供该额外要求。

触发与影响：即使配对与调度 runtime 正常就绪，通过 AkSchedEnqueueSpec 调用 SchedRuntimeFfi.enqueue 仍会在提交前收到 AK_ERR_INVALID。绑定类型无法表达引擎要求的合法输入，整个公共调度发送接口不可用；这不影响另外的 legacy GUI send 入口，不能据此声称所有 GUI 发送失败。

修复建议：冻结实际端点来源并统一 schema，优先由引擎从已认证会话解析端点；若当前适配器确需显式地址，应补齐契约与 Dart 类型，继续从 Ready relation 决定真实身份和 pin。

覆盖缺口：`flutter_app/packages/ak_ffi_dart/test/pairing_ffi_test.dart:249`、`:681` 只检查示例和 Fake 受理。后续覆盖 Dart 构造规格→真实导出→关系解析→入队，不能只分别测试各层。

## P2 建议（13 项）

### P2-01：CLI 接受 IPv6 候选，但发送 socket 始终绑定 IPv4

**归属：既存 CLI 限制，本轮新增多地址输入仍未补齐。置信度：高。**

证据：`crates/ak-cli/src/main.rs:559` 接受 `[v6]:port`，`:4233` 的解析测试也将 IPv6/混合地址视为有效；但每次发送都在 `:2775` 设置 `0.0.0.0:port`。`crates/ak-engine/src/lib.rs:1108`、`:1109` 会在连接前拒绝本地与对端地址族不匹配。

触发与影响：候选中唯一可达地址为 IPv6，或直接使用 `--to [::1]:端口`，都会快速收到 address family mismatch；IPv4 候选失败后无法实际回退到 IPv6。成功解析和显示候选并不代表发送入口具备相应连接能力。

修复建议：按每个候选的地址族选择 bind，或使用已经明确配置并验证的双栈端点，同时处理引擎的地址族快速校验。保留显式源端口语义。后续补真实 CLI 的 IPv6-only 和 IPv4 失败→IPv6 成功场景；当前新增测试只验证 IPv6 参数解析，真实回退测试仍为 IPv4 环回。

### P2-02：接收端候选枚举忽略实际 bind 地址，公布了没有监听的端点

**归属：第二十五波新增。置信度：高。**

证据：`crates/ak-cli/src/main.rs:2409` 仅把实际监听地址的 port 传给 address_candidates；`:2286` 的 API 也只有 port；`:2252` 枚举所有非环回、非链路本地 IPv4 接口，没有收到或检查监听 IP/地址族。

触发与影响：`receive --bind` 只绑定某一网卡地址、环回地址或 IPv6 地址时，输出仍把其他 IPv4 网卡的同端口列为“try these”。发送方或读取该输出的脚本会尝试实际未监听的地址，产生无谓超时或错误诊断。daemon 当前固定绑定 IPv4 wildcard，本项主要影响支持显式 bind 的 receive 入口。

修复建议：候选函数接收实际绑定端点及双栈能力；具体 IP 只公布该端点，wildcard 才展开兼容接口，显式保留环回/IPv6 的准确表达。后续覆盖具体网卡、环回和 IPv6 bind；当前 candidates_from_nets 的过滤测试没有验证监听约束。

### P2-03：下调队列限额后，旧积压被永久保留为实际容量

**归属：全局逻辑既存，第二十四波新增每码上限继承此缺陷。置信度：高。**

证据：`crates/ak-session/src/scheduler/queue.rs:104` 将全局容量设置为 `max(请求容量, 当前长度)`；`:116`、`:117` 对每码容量取 `max(请求容量, 最大现存组长度)`。后续 `:155`、`:158` 只检查这两个放大后的值，没有自然下调逻辑。`crates/ak-session/src/scheduler/core.rs:550`、`:554` 却对外保存用户提交的新 policy。

触发与影响：A 码有 3 个排队任务时将每码上限从 4 改为 1，随后排空，再次提交时仍允许排入 3 个；其他码也继承这个放大的每码容量。全局容量同理。快照显示新小值，实际背压使用旧大值，不符合 `docs/contracts/scheduler.md:222`、`:223` 的队列限制。

修复建议：保存请求的目标容量即可，不必删除已有积压；新增入队在当前长度不低于目标容量时拒绝，随着出队自然收敛。后续覆盖“缩小→保留积压→排空→再次入队”，当前只检查缩小时不丢旧任务的测试不足以证明配置生效。

### P2-04：跨码 serial、同码 parallel 缺少有限批次，热码可让其他码无限等待

**归属：既存调度契约缺口，本轮模式测试仍未覆盖。置信度：高。**

证据：`docs/contracts/scheduler.md:234` 要求跨码 serial 采用有限批次，后续追加进入下一批。`crates/ak-session/src/scheduler/core.rs:288` 每次 pump 逐组尽可能补满；`crates/ak-session/src/scheduler/admission.rs:203` 拒绝与当前活跃组不同的组，`:209` 允许当前组有空位便继续准入，没有冻结本批任务集合。

触发与影响：A 码两个并行任务交错结束，B 已排队，同时不断向 A 追加。每完成一个 A 就补一个 A，A 始终有活动任务，B 永远不能进入。组间队列轮转无法消除这一条件下的饥饿。

修复建议：跨码 serial 选中组时记录本批可执行任务集合或入队序号截止点；后续追加归下一批，当前批结束后轮转。`crates/ak-itests/tests/dual_scheduler_modes.rs:452` 的公平性测试只覆盖 serial/serial；后续增加 parallel/serial、持续追加及不同任务耗时的活动区间验证。

### P2-05：FFI JSON 策略入口静默忽略新增三项限额，快照也未返回它们

**归属：第二十四波跨层接线遗漏。置信度：高。**

证据：`docs/contracts/scheduler.md` §3 已冻结 max_active_tasks、max_queued_tasks_per_code、max_pending_admissions。`crates/ak-ffi/src/pairing.rs:1295` 的 parse_policy 仅解析原有模式和三个旧限额，在 `:1327` 调用旧五参数 SchedulerPolicy::new；`:1558` 的快照序列化同样缺少新三项。`crates/ak-session/src/scheduler/config.rs:250` 明确说明旧 constructor 的兼容默认行为，但未授权 JSON 入口忽略显式的新字段。

触发与影响：C 调用方提交包含 `max_active_tasks:1` 的合法新 policy，set_policy 可返回成功，内部仍使用默认 16；每码排队和 pending admission 的显式值也不起作用。Dart policy 类型虽包含八个字段，但 `flutter_app/packages/ak_ffi_dart/lib/src/model/sched_policy.dart:152` 的 toJson 只发送旧五字段，`:177` 的 fromJson 又为快照缺失项补默认值，GUI 因而无法可靠配置或展示真实的新上限。此项与 P0 的 ABI 崩溃独立：即使只用正确参数的 C 调用，策略值仍会被忽略。

修复建议：共享一个版本化 policy schema、解析和序列化规则，将新三字段贯穿 Rust FFI、Dart 模型及快照；不支持的版本/字段应明确拒绝，不能成功后按另一套值执行。后续通过真实 FFI 设置非默认值并读取原生结果，核对实际准入。默认值与契约示例数字不同本身不计为缺陷。

### P2-06：常驻 daemon 永久保留每次接收的 executor 和历史 entry，内存随运行次数增长

**归属：既存 runtime 历史保留策略在第二十四波常驻真实接收中放大。置信度：高。**

证据：`crates/ak-cli/src/daemon.rs:381` 每轮创建新的 RealTransferExecutor，`:395` 放进 TransferSpec，`:696` 持续重复。`crates/ak-session/src/runtime/actor.rs:101` 的 TaskEntry 持有完整 spec，`:283` 持续插入 entries；`:457` 完成处理仅清除 inflight/driver，没有移除 entry 或释放其 executor。core 也保留任务记录；actor `:785` 的 snapshot 会遍历全部历史。`real_executor.rs:56` 的 outcome 数量上限只针对单个 executor，无法限制“一轮一个 executor”的总量。

触发与影响：常驻 daemon 经历大量成功、拒绝、超时或错误接收后，历史 task、executor、observer、结果等一直驻留，快照遍历/排序成本也增长。排队上限只限制等待任务，无法约束这部分保留内存。

修复建议：终态后释放工作 spec/executor，保留有界的精简结果或持久化历史；可重试任务明确保留必要信息，查询采用分页。后续连续结束大量任务，检查活跃 executor/内存历史条目数量能收敛，不能只验证单个 executor 的 outcome cap。

### P2-07：FFI enqueue 忽略 request_id，重试同一请求会创建不同真实任务

**归属：既存幂等契约缺口，当前仍在。置信度：高。**

证据：`docs/contracts/ffi-v2.md:147` 要求新操作以 request_id 幂等；`flutter_app/packages/ak_ffi_dart/lib/src/sched_ffi.dart:163` 确实输出它。但 `crates/ak-ffi/src/pairing.rs:1346` 的 enqueue 不读取 request_id，`:1429` 每次 random_id16，`:1435` 登记新 engine spec，`:1451` 重新提交。

触发与影响：直接 C 调用方提供齐全规格，对同一请求重试，会得到不同 task id，产生重复扫描/发送和文件冲突处理；Dart 的 P1-06 修复后也会暴露该行为。不能把创建邀请码的局部幂等实现外推为调度入队幂等。

修复建议：以 runtime 内 request_id 和规范化规格摘要登记受理结果，相同请求返回原 task id，不同内容复用同一 ID 时明确拒绝；去重必须先于登记 executor spec 和提交副作用。后续覆盖 enqueue 重放及相同 request_id 不同内容。`crates/ak-ffi/tests/ffi_pairing.rs:413` 现有幂等测试只覆盖已完成 create 后再次 create。

### P2-08：新配对/调度 Dart 绑定丢弃同步 native 错误详情

**归属：既存错误传播缺口，当前仍在。置信度：高。**

证据：`flutter_app/packages/ak_ffi_dart/lib/src/pairing_ffi.dart:824`、`:909` 及 `flutter_app/packages/ak_ffi_dart/lib/src/sched_ffi.dart:411`、`:423` 的负返回码只转成固定文案和状态名；对应原生绑定未读取 ak_engine_last_error。Rust `crates/ak-ffi/src/pairing.rs:372` 对目录创建/密钥读取失败提供具体原因，调用方最终只看到 AK_ERR_INVALID 等通用码。已有普通引擎绑定 `flutter_app/packages/ak_ffi_dart/lib/src/engine.dart:489` 已实现读取详情的模式。

触发与影响：身份目录不可写、规格被 native 拒绝、调度受理失败等情况，用户和维护者无法从 UI 得到根因；同一状态码覆盖多种错误时无法给出准确恢复建议。异步 op 自带的错误 JSON 不代表同步错误也已完整传播。

修复建议：负返回码后、下一次 native 调用之前立即复制线程局部 last-error，复用现有引擎绑定能力；Fake 调用接口同时表达状态码和文本。后续验证真实 native 错误经 Dart/service 到 UI 的内容，而不仅是异常类型或码值。

### P2-09：跨设备脚本在普通失败或中断时可能删除失败现场

**归属：第二十五波新增脚本。置信度：高。**

证据：`scripts/cross_device_test.sh:62` 启用 `set -euo pipefail`，没有 errtrace；`:201` 依赖 ERR trap 设置 FAILED。真实流程在 real_run 函数内，例如 `:631` 的 keygen 和 `:648` 的 invite create 直接失败时，ERR trap 未继承到函数内部。另 `:236` 的 INT/TERM trap 明确直接 exit 130，没有设置 FAILED。EXIT cleanup 在 `:216`、`:225` 仅检查 FAILED/KEEP，`:231` 可删除工作目录，没有读取原始退出状态。

触发与影响：注册服务不可用使 invite create 失败，或用户中断运行，脚本可能按成功路径清除本地日志和远端测试目录，违背 `:57` 的失败保留承诺。涉及的是脚本生成的测试现场，未据此推断正常用户文件被删除。

修复建议：EXIT trap 第一行保存原始退出码，非零即保留现场；errtrace 仅辅助定位，不作为失败保留的唯一依据；中断显式记录失败。后续用命令失败和 SIGINT 两类流程覆盖清理行为，当前 self-test 只覆盖参数/解析等成功路径。

### P2-10：远端公布候选非空但不可达时，跨设备脚本不尝试已知公网入口

**归属：第二十五波新增脚本。置信度：高。**

证据：`scripts/cross_device_test.sh:695` 注释称 SSH host 是最后回退，但 `:699` 到 `:703` 只在公布候选为空时设置该地址；`:734` 随后只遍历 REMOTE_CANDS。

触发与影响：远端 Linux 位于一对一 NAT 后，网卡只枚举私网 IP，SSH 使用公网入口且该公网 UDP 端口可直达。候选非空使公网入口被遗漏，脚本把正常可传输的环境报告为 all remote candidates failed。回程的 SSH_CONNECTION 候选逻辑不能补救正向地址集合的遗漏。

修复建议：将明确可用于数据面的远端地址覆盖参数，或正确格式化的 SSH host 候选，保序追加到列表末尾并去重；若 SSH host 是跳板而不是接收设备，必须使用独立数据地址，不能默认把跳板当接收方。后续覆盖“公布列表非空且全部失败、外部地址可达”的情况；现有候选解析 fixture 没有覆盖候选构造和尝试集合。

### P2-11：跨设备脚本的 IPv6 端点拼接与无 Python 解析路径不完整

**归属：第二十五波新增脚本边界问题。置信度：高；无 Python 的解析子场景依赖输入日志包含 IPv6。**

证据：`scripts/cross_device_test.sh:170` 以 `host:port` 拼接 registry，`:701`、`:718` 同样拼接 SSH host/SSH_CONNECTION 的 IP，未给 IPv6 字面地址加方括号。`:418` 的备用 sed 模式用第一个 `]` 结束 candidates 数组匹配，遇到 `"[2001:db8::a]:7300"` 时会在地址内部截断；`:421` 仅凭非空就返回解析成功。`:265` 的 sshpass 后端不要求 Python，`:535` 的 fixture 已包含 IPv6。

触发与影响：以裸 IPv6 字面地址作为 server-host 或 SSH 对端时，生成的端点不能由 Rust SocketAddr 解析；无 Python 且日志带 IPv6 候选时，会输出残缺地址并阻止正常回退。当前 daemon 的新候选块只产生 IPv4，故不能把 sed 子问题写成每次真机运行必现；端点拼接问题仍可独立触发。

修复建议：集中实现端点格式化及合法性检查；JSON 使用可靠解析器，或明确要求解析依赖，并在不支持的输入形状上失败，不返回截断地址。后续覆盖 IPv6-only host、IPv6 SSH_CONNECTION 及无 Python 候选输入；不能仅断言解析结果非空。

### P2-12：Linux 目标检查脚本已经看到纯 Rust 子集失败，仍可能返回成功退出码

**归属：第二十四波新增检查脚本。置信度：高。**

证据：`scripts/check_linux_targets.sh:74` 遇到 custom build command 失败后设置 BLOCKED，`:78` 补查纯 Rust 子集；但 `:81` 的失败分支只追加 note，没有把 status 改为 FAIL。`:97` 的汇总因此仍显示 FAIL=0，`:104`、`:105` 返回 0。

触发与影响：全依赖检查先被 C 工具链问题挡住，随后独立纯 Rust 检查发现 ak-core/ak-wire 的代码错误，调用方仍收到“环境受阻、非失败”的机器结果，漏报脚本自身已经观测到的失败。这是返回码分类问题，本次没有执行该检查。

修复建议：纯子集失败应升级为 FAIL；若该次也可能是环境问题，应针对该次日志独立分类，不能沿用前一次状态。用假命令/日志 fixture 验证组合分类即可。另 `:41` 在 nounset 下直接读取 USERPROFILE，迁移到普通 Linux 环境时应提供未定义处理；鉴于该脚本主要面向 Windows 交叉检查，此点只作同项可移植性建议。

### P2-13：当前状态文档互相矛盾，测试数字与代码版本没有统一对应

**归属：第二十五波现状更新不完整，夹杂更早的过期描述。置信度：高。**

证据：

- `docs/HANDOFF.zh-CN.md:49` 已描述第二十五波跨设备、19 导出及真实 executor；同文件 `:80`、`:85` 的下一步/结尾仍使用 Fake executor、14 导出和未部署等旧状态。`:48` 明确标为第二十二波的历史说明可以保留，不能与当前指引混用。
- `README.md:24` 仍称当前全部验证为本机环回、跨机器真实传输未完成；`docs/SUPPORT_MATRIX.zh-CN.md:20`、`:51` 仍称没有 Linux 主机运行证据，与新 HANDOFF 的 Linux CLI 运行记录冲突。Linux CLI 与 Linux Flutter 必须分别描述。
- `docs/ROADMAP.zh-CN.md:11`、`:13` 仍是第二十四波接线进行中和 918 项测试；`reports/dev-validation-2026-11-02.txt:562` 的 918/0 属第二十三波，`:576` 的第二十四波记录为 955/0。现有计数不能被无条件写成最新 HEAD 的统一验证结果。
- `CHANGELOG.md:47` 的“外部 Codex 终审全部发现修复关闭”也需收窄；`docs/BACKLOG.zh-CN.md:161` 实际保留了前次两项 P2 边界，见后文复核。

影响：后续执行者难以判断剩余工作和验证覆盖，可能重做已完成工作，也可能把 CLI 实测外推为 GUI/调度全链验收。此项不否定用户提供的历史全绿及跨设备结果；问题是仓库内部的证据版本和当前指引不一致。

修复建议：保留明确带波次的历史记录，统一刷新 README、HANDOFF 当前指引、ROADMAP 和支持矩阵；测试数字附提交 SHA、平台、特性和记录位置。区分“真实文件经过本地 executor”“双端准入与真实分组完成”“该功能真机验证”三个不同结论。后续以文档交叉核对完成此项，不需要为文字修改增加实现镜像测试。

## 冻结契约、前次发现和覆盖范围复核

下表区分静态相符、已计入发现和既有未完成事项，避免把设计文件、测试代码存在和历史运行结果混为一谈。

| 范围 | 本次静态结论 | 证据与界限 |
|---|---|---|
| ADR-001 与数据路径 | 阅读到的候选回退仍调用设备直连引擎，没有发现本轮新增 TURN、HTTP 文件上传或 relay 回退 | 身份/路径仍由真实 DirectChannel 与 gate 校验；本次没有网络实验或抓包，不宣称 L3/L4 无中继证明 |
| 多 IP 枚举/排序/回退 | IPv4 去重、稳定 LAN 提示排序、全失败原因汇总基本成立；超时和 socket 约束见 P1-01、P2-01/02 | RFC1918/接口信息只是候选排序提示，不是路径或身份的密码学证明；连接后失败停止回退是明确选择，不单独判为缺陷 |
| Dart registry_id | 当前实参已对齐 64 hex；默认配置、环境配置及 Android 测试入口一致 | `flutter_app/packages/ak_ffi_dart/lib/src/pairing_ffi.dart` 与 `flutter_app/lib/services/invite_service.dart:193`；未发现需要补报的 32-hex 实参问题 |
| protocol-v2.md | 控制协议 30 个 tag、预算常量及 8 个 PairError 数值抽查未见漂移 | 未因此证明所有状态转移和恢复时序正确；此次没有重跑编解码或配对测试 |
| scheduler.md | wire/net 三个准入帧预算已一致为 113 / 38 / 32；三限额已进入 scheduler 核心 | 双端生产行为、生命周期、公平性和动态配置仍有 P1-02/03、P2-03/04/05 问题；不能只按字段和预算相符判契约完成 |
| ffi-v2.md | 19 个实际 v2 导出名称含兼容入口，并不等于冻结 ABI 与 Dart 调用一致 | P0-01 的真实参数错配；P1-06 的 JSON 输入缺口；P2-05/07 的策略与幂等遗漏。已披露的 FFI listener_disabled、尚未接 wire admission、Dart 未绑定部分新增导出不再重复计数 |
| pairing-flow.md | 64-hex 层面没有新增调用方漂移；配对运行时 stop 行为存在 P1-04 的终态反转 | 既有 owner 握手窗口恢复限制仍按 SECURITY 的已披露边界处理，不重复包装成新增发现 |
| storage-schema.md | 模型/记录格式存在，不代表生产 scheduler 已接入全部恢复行为 | 契约 §7 的源/目标引用、request/options 等恢复信息尚未完整进入 `crates/ak-store/src/queue.rs:154` 的 QueueTaskInit；本次阅读到的 runtime 未使用 TaskQueueStore。归入现有生产调度未完成边界，不另计一个“存储已损坏”发现 |
| Linux Flutter 平台 | crate 的 cdylib 名、CMake 的 libak_ffi.so 位置及 Dart 加载名称静态相符，未发现确定性接线冲突 | `flutter_app/linux/CMakeLists.txt:133`、`flutter_app/lib/services/dll_path.dart:70`；path_provider_linux 使用 Dart 插件注册，C++ registrant 为空本身不是缺陷。本次未构建 Linux GUI |
| check_contracts.py | 其五类比较有助于发现预算、符号集合、字段名、错误码漂移 | 不比较 C 参数签名、实际 Dart 调用、状态机或恢复行为。`:425` 明确 advisory、FAIL 仍 exit 0 是现有设计，不另计缺陷；“5/5 PASS”不能覆盖 P0-01 |
| cross_device_test.sh | 可以形成配对及双向字节/摘要一致的历史证据，但不是全部新增路径的验证 | `:734`、`:738` 的循环每次只传一个 `--to`，没有在真机上直接覆盖 CLI 逗号多候选驱动；`:639` 的本地 daemon 使用 `--expect any`，该方向不验证配对簿拒绝未知发送方。脚本 6 步含准备和汇总，不能当作六个并发/故障场景 |

前次 `reports/codex-review-2026-11-05.md` 第 5 项仍有两条明确未完成，`docs/BACKLOG.zh-CN.md:161` 已披露，本次不重新计数：

1. **on-channel 在准入前扫描。** `crates/ak-engine/src/lib.rs:2160` 仍先 prepare_send，`:2071`、`:2084` 执行源扫描和发送检查点写入，`:2232` 才进入 admission handshake，尚不满足 `docs/contracts/scheduler.md:165`、`:205` 的准入后扫描顺序。后续应拆分握手与准备，验证准入拒绝时不扫描、不写新检查点。它与 P1-01 共享扫描阶段，但 P1-01 是本轮新 timeout 造成的正常 CLI 回归，独立计数合理。
2. **引擎检查点仍未接 AKREC2 传输实例隔离。** `crates/ak-engine/src/lib.rs:620` 的 record_id_for 仍以 AKREC1 和内容/对端/方向/根派生，不包含 transfer_id；发送 `:2080`、接收 `:2843` 仍调用它。同内容、同对端、同根的独立任务仍共用记录键，staging task ID 不同不能消除该碰撞。后续需把持久 transfer_id 贯穿引擎并验证“不同实例隔离、同实例重启复用”。

前次路径防护、命名空间和并发提交项目，本轮阅读能看到 acbaf27 的对应收窄与修复；本次没有重复报告已修复的原触发路径，也没有将路径级复查等同于消除全部文件系统竞态。已披露边界应继续按原承诺范围保留。

## 修复与后续验证顺序

先处理 P0-01，统一 FFI 声明并覆盖真实 GUI 策略调用；同时收敛 P1-04/05 的配对与 Dart 句柄生命周期。然后修复 P1-01 的连接预算和 P1-03 的真实任务关闭，再完成 P1-02 的生产分组/双端准入和 P1-06 的 enqueue schema。P2 可分别按 CLI、scheduler、FFI、脚本及文档模块并行推进，跨模块变更先冻结公共签名与 JSON。

后续验证应直接针对发现中的触发条件：真实 DLL/so 的调用与释放时序、慢扫描的发送、多任务真实活动区间、真实 executor 的停止/端口释放、动态限额与热码公平性、脚本失败保留与地址构造。此处只给出验收建议；本次没有执行这些验证，也没有修改实现、测试或现状文档。

**最终计数：P0 1，P1 6，P2 13，共 20 项；前次已披露且仍开放的 2 项 P2 仅作复核，不计入本次 20 项。**
