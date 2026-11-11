# AK-sending 全项目终审报告（第三十六波）

评审日期：2026-11-09。评审基线：`37a57c0b4a5897b49c6166827419d2008e65618e`。范围：Windows/Linux CLI、Windows GUI、Android GUI、registry v2、旧版 signal/STUN，以及共享的传输、安全、存储、FFI、Dart 桥和打包链。

## 一、执行摘要

**结论：当前基线不宜判定为全项目终审通过。** 本轮静态评审登记 **13 项发现：P0 0 项、P1 5 项、P2 8 项**。最直接的用户功能阻断是 CLI 的 v1 任务码等待链没有保活：registry v2 会在完整入站帧静默 30 秒后断开，而 CLI 仍可继续显示尚未过期的任务码。wave-35 的 FFI keeper 没有覆盖 CLI，且 FFI 自身在一次重连批次全部失败后会停留在 Connecting，无法自行恢复。

另外三个 P1 分别涉及：registry v2 握手失败泄漏连接配额，CLI 将 stdin EOF/读取错误解释成同意接收，以及旧版 signal 缺少完整的连接与阻塞 I/O 资源边界。P2 涉及终端输入任务回收、GUI 故障闩与显示、同设备多实例争抢控制连接、claim 幂等记录回收后的错误行为、常量与测试断言冲突、Android 产物来源门槛和文档漂移。

在本轮阅读的生产路径中，未发现新增 TURN/relay、HTTP 文件上传、云端文件暂存或隐藏代理回退；未发现可仅凭陌生 code_id、伪造 endpoint blob 或错误指纹进入文件数据通道的新绕过。配对证明、QUIC 指纹钉扎、方向前置门禁、Commit 时兑换额度复查，以及引擎的 consent 拒绝路径具有对应实现。以上是源码结论，不是对网络实际路由或发布二进制的证明。历史已披露的符号链接竞态、Windows 无硬链接卷的提交回退仍需保留为限制，不能解释为已经消除，详见平台与共享层小节。

### 证据口径与执行边界

本轮先读取 README、HANDOFF、ROADMAP、AGENTS 和 ADR-001，再沿调用链阅读生产代码、测试源文件、契约、构建与打包脚本及 Git 历史。工作树开始时干净；本轮仅新增本报告，未修改实现、测试或其他文档。按要求**没有执行任何构建、测试、lint、仓库检查脚本、网络复现、真机操作或部署**。报告中的触发过程均为静态推导；“已有测试”仅表示阅读到对应测试源文件。

仓库要求按模块并行评审；当前工具未提供可调用的子代理/协作接口，因此独立读取按模块批量并行执行，再统一交叉核对，未宣称存在额外独立评审者。

| 证据类型 | 本轮如何使用 |
|---|---|
| 用户提供的历史验证 | workspace 1132/0、Dart 161/161、Flutter 165/165、契约 5/5、跨设备真机双向 SHA256 一致、wave-25/28 发现关闭、wave-33 独立审核通过；作为历史背景保留，不当作当前 HEAD 的重新执行结果 |
| 当前源码与文档 | 支持下述静态发现、调用链判断与契约差异；每项列出定位 |
| 当前产物 | 读取到两种 Android ABI 的受跟踪 `.so` 及刷新提交历史；未重新构建、加载、执行或验证 APK 内部产物，源码与二进制等价性未证实 |
| 本轮实际执行 | 仅文件/Git 读取与报告写入；构建 0、测试 0、动态安全复现 0 |

严重性定义：P0 为灾难性数据/信任边界破坏或架构红线；P1 为核心流程阻断、明确授权错误或服务器显著可用性缺陷；P2 为条件性正确性、生命周期、交付与证据一致性问题。置信度说明静态结论的可靠程度，不代表问题已在真机复现。

## 二、分级发现

### P0

本轮未确认 P0。该表述不构成“全仓库不存在 P0”的证明。

### R36-P1-01：CLI 任务码等待链没有保活或重连，30 秒静默后 owner 离线

- **定位**：`crates/ak-cli/src/task_cli.rs:512`、`:539`、`:609`、`:1480`、`:1627`、`:1705`；`crates/ak-signal/src/server_v2/engine.rs:91`、`:510`、`:597`；`crates/ak-session/src/pairing/task.rs:1050`。
- **证据与触发**：CLI TaskStack 只有 pairing、owner_loop 和 link_rx；open_session 建立 RegistryClient 后没有 ping、状态 watcher 或 keeper。code-send/code-recv 发码后进入 wait_paired，只等待配对、任务事件、取消与码 TTL。服务器 reader_loop 对后续每个完整帧使用 30 秒 timeout，即使一个字节也没有收到同样超时；出站消息不会刷新该入站期限。TaskPairing 已有意移除“控制连接断开即终结所有任务”的映射，因此断线也不会使这里立即退出。
- **影响**：正常用户分享任务码后超过约 30 秒才加入，就可能遇到 owner 不在线/配对不可用；发码 CLI 仍显示等待，直到默认 600 秒 TTL 或取消。四个 task 命令共享的会话栈均无维护策略，最直接受损的是两种 code-* owner 等待场景。已建立的文件直连不应因此被判定必然中断。
- **修复建议**：把 registry 保活、断线恢复和退出取消收敛为可复用的会话生命周期组件，同时接入 CLI 与 FFI；保活间隔必须留足 30 秒预算余量，恢复失败应有可观察状态。保持控制面故障与健康数据直连的独立性，不引入中继回退。
- **置信度**：高。完整调用链中未找到 CLI 等价保活；时限来自服务器生产常量。本轮未执行延迟加入复现。

### R36-P1-02：重连耗尽后状态遗留为 Connecting，FFI keeper 无法再次恢复

- **定位**：`crates/ak-session/src/pairing/client.rs:1004`、`:1010`、`:1026`、`:1090`、`:1286`；`crates/ak-ffi/src/task.rs:1294`、`:1308`、`:1314`；`crates/ak-cli/src/daemon.rs:1455`。
- **证据与触发**：RegistryClient::reconnect 先发布 Connecting，默认尝试 4 次；所有 open_connection 尝试失败后直接返回 Err，没有发布 Disconnected/可重试终态。FFI keeper 只在 watcher 值为 Disconnected 时调用 reconnect，并忽略返回错误；其后看到 Connecting 不再重连，定时 ping 又因 connected=false 返回 Offline，仍不改状态。daemon 的同类 keeper 也受影响，其“失败会再次设置 Disconnected”的注释与实现不符。
- **影响**：网络/服务器故障持续超过一次重连批次后，即使随后恢复，等待中的任务码和后续登记/加入也可能一直不可用，需重建运行时或重启应用。该结论覆盖 Windows/Android GUI，也覆盖复用相同客户端的 CLI daemon。
- **补充边界**：ping 的请求超时只返回 Timeout，客户端 reader 没有自身静默期限；keeper 忽略 ping 错误。因此 TCP 尚未报告失败的黑洞连接也缺少应用层主动恢复闭环，其持续时间依赖操作系统 TCP 行为，未动态测量。
- **修复建议**：定义明确的重连状态机与重试调度；一批失败后返回可重试状态并按有上限的退避继续，支持退出取消、避免无间隔自旋；连续 ping 超时应触发有代际保护的断连/重连。不要仅修改注释。
- **置信度**：高；黑洞持续时间为环境相关。成功重连时订阅本身会保留，本项不是“owner-loop 每次断线都会退出”。

### R36-P1-03：registry v2 的失败握手泄漏连接配额，过载握手又绕过实际并发上限

- **定位**：`crates/ak-signal/src/server_v2/engine.rs:86`、`:168`、`:183`、`:190`、`:202`、`:208`、`:232`。
- **证据与触发**：accept 时 conn_count 加一并分配任务号；TLS 失败、TLS 超时、ALPN 校验失败均提前 return。计数减一及 conn_tasks 移除只位于成功进入 connection_loop 后的分支，没有覆盖这些提前退出路径的 RAII 清理。超过 MAX_LIVE_CONNECTIONS 的连接虽然撤回计数，仍 spawn 并执行 TLS 握手，且该分支的 JoinHandle 不进入 conn_tasks。
- **影响**：累计 256 个被计数但握手失败的连接即可耗尽准入名额，后续合法客户端持续收到连接上限错误，直至进程重启；不要求攻击者通过设备身份认证。握手失败也可能自然发生。已经超额时，新握手任务仍可继续堆积，名义 256 上限没有约束这部分 CPU/socket/任务占用，关闭流程也不能统一追踪它们。
- **修复建议**：以 RAII permit 管理接入槽，覆盖握手失败、ALPN 失败、取消及正常退出；在 spawn/握手前执行真正有界的准入，必要的拒绝握手使用独立小预算；统一追踪任务，关闭时取消并回收。只给正常 connection_loop 增补超时不能修复此处。
- **置信度**：高。计数全部引用及提前返回路径已核对。已有 `connections_beyond_the_global_cap_are_rejected_with_server_error`（`:2745`）测试源使用成功 TLS 连接占满名额，不覆盖失败握手后的配额返还。

### R36-P1-04：CLI 把 stdin EOF、读取错误及读取任务错误解释成同意接收

- **定位**：`crates/ak-cli/src/task_cli.rs:913`、`:921`、`:924`、`:947`；`crates/ak-engine/src/lib.rs:3544`。
- **证据与触发**：Terminal 分支对 spawn_blocking 的结果使用 unwrap_or_default；read_stdin_line 对 Ok(0) EOF 返回空串，对读取错误也返回空串；后续把空串与 y/yes 一起映射为 true。这里没有区分“用户主动按 Enter”与“根本没有可读输入”。
- **影响**：code-recv/join-recv 在关闭 stdin、空管道、非交互运行或输入读取失败时，可未经当次明确决定立即同意合法配对端的文件 Offer，并开始落盘。它不绕过配对身份，但破坏逐次 consent 的用户授权边界。
- **修复建议**：保留读取结果及字节数；EOF、I/O 错误和 JoinError 一律拒绝。若保留 Enter 默认同意，必须只接受确实读取到的换行；更明确的产品语义是只接受 y/yes。自动接受如有需求应为显式选项，不能由输入异常隐式触发。
- **置信度**：高。此项与正常 consent=false 的零用户文件内容写入保证并不矛盾：错误发生在 CLI 产生 true 的位置。

### R36-P1-05：旧版 signal 存在无界半开连接及可传播到服务主循环的阻塞

- **定位**：`crates/ak-signal/src/server.rs:219`、`:227`、`:231`、`:386`、`:459`、`:473`、`:507`、`:514`、`:525`。
- **证据与触发**：每个 TCP accept 直接 spawn，没有全局连接/握手上限；read_frame 没有首帧或后续帧期限。每连接虽有 64 深度 outbox，deliver 却无限等待 send，writer.write_all 也无期限。主 accept/sweep 循环逐条 await 过期通知；当某慢客户端写端及队列阻塞时，通知可以让主循环停止处理 accept、下一次 sweep 和 shutdown。末尾对 writer_task 的 timeout 在超时后仅丢弃 JoinHandle，没有 abort 仍运行的 writer。
- **影响**：未认证的静默连接即可长期占用 socket 与后台任务；经认证的慢读端还可把队列背压扩散至其他连接乃至服务主循环。帧频率桶与 session TTL 都不约束“尚未发送 Hello”或“写端阻塞”这一阶段。
- **修复建议**：为旧 signal 独立补全接入配额、首帧/帧 I/O 截止期、非阻塞或有时限的 outbox 投递、慢端主动关闭与任务追踪；关闭时显式 abort/join。共享主循环不得等待单个客户端无限排队。
- **置信度**：高；主循环停顿要求慢端的 socket 与 outbox 实际饱和，具体时间未测量。此项针对旧 signal，不能用 registry v2 已有的帧期限替它背书。

### R36-P2-01：CLI consent 超时后遗留不可取消的 stdin 阻塞任务

- **定位**：`crates/ak-cli/src/task_cli.rs:845`、`:920`、`:932`、`:944`、`:697`；`crates/ak-cli/src/main.rs:3768`。
- **证据与影响**：每次 Offer 启动一个 spawn_blocking(read_stdin_line)，超时分支胜出只丢弃 handle，已经执行的阻塞读取继续存在。接收器在拒绝后继续监听；后续新 Offer 可再创建读取者，旧读取可能消耗新提示的答案。常规 main 返回时，Tokio runtime 关闭还可能等待未结束的阻塞任务，使已经显示结束的命令不退出。源码“超时后未读输入无害，进程随任务退出”的注释不成立；显式 process::exit 路径不受 runtime 等待影响，但不能覆盖所有正常退出。
- **修复建议**：采用一个受会话管理的终端输入源，将输入与当前 consent 请求关联；过期后丢弃对应输入，不为每个 Offer 新建不可回收阻塞读。为普通退出、超时重试和 Ctrl-C 分别定义退出语义。
- **置信度**：高（任务残留机制）；具体终端与退出表现为中高置信度，未执行真实 stdin 场景。现有 Scripted/WindowTimeout 测试分支绕过真实 Terminal 读取。

### R36-P2-02：进度轮询异常计数被健康事件轮询清零，C12 故障闩无法触发

- **定位**：`flutter_app/lib/services/simple_transfer_service.dart:727`、`:741`、`:903`、`:1079`、`:1108`。
- **证据与影响**：drainProgress 捕获 AkFfiException 后调用 _notePollFailure，却返回 malformed（没有坏 JSON 时为 0）；_pumpOnce 仅依据 malformed==0 就清零失败计数。事件通道健康、进度原生接口持续失败时，每个常规 tick 都是 0→1→0，永远达不到 10 次阈值。服务继续轮询且进度停滞，不符合 C12 的失败升级语义。
- **修复建议**：让 drainProgress 返回明确的轮询成功/失败结果；只有事件与进度两个通道本 tick 都成功才清零。保留 StateError 租约撤销的即时终止路径。
- **置信度**：高。已有测试源 `flutter_app/test/simple_transfer_service_test.dart:1087` 覆盖事件失败，`:1145` 覆盖坏 JSON，`:1188` 覆盖定时事件失败，不能证明进度原生异常这一组合正确。

### R36-P2-03：engineFailed 只改变内部字段，没有主动送达真实 GUI

- **定位**：`flutter_app/lib/services/simple_transfer_service.dart:869`、`:913`；`flutter_app/lib/services/ffi_simple_transfer_service.dart:236`、`:315`；`flutter_app/lib/ui/service/simple_transfer_service.dart:151`；`flutter_app/lib/ui/pages/home_page.dart:53`。
- **证据与影响**：_latchEngineFailed 只记录 failureReason/phase 并停止泵，_setPhase 不发通知；UI service 契约没有健康/失败状态流，真实 adapter 也不转发 isEngineFailed/failureReason。主页和配对控制器只监听连接快照；故障发生时快照不会因此更新。等待中的用户可继续看到旧的等待/连接状态，直至后续操作抛错或其他超时。接收等待器只检查 isDisposed，也不会立即显示闩上的具体故障。
- **修复建议**：在服务契约增加可回放的健康状态/故障通知，真实 adapter 转发，页面展示可恢复提示；待处理操作和接收等待及时按同一故障结束。只暴露供单元测试读取的 getter 不足以实现用户可见失败。
- **置信度**：高。现有 C12 测试中的“visible”主要断言内部 service 属性，不是生产页面显示。本项独立于 P2-02：即使事件失败已经成功触发闩，仍有显示缺口。

### R36-P2-04：同设备多个 keeper 会争抢唯一 registry 连接，新 CLI task 也未复用常驻连接

- **定位**：`crates/ak-signal/src/server_v2/engine.rs:916`；`crates/ak-ffi/src/task.rs:1314`；`crates/ak-cli/src/daemon.rs:1439`；`crates/ak-cli/src/task_cli.rs:539`；`crates/ak-cli/src/main.rs:654`；`flutter_app/lib/services/ffi_simple_transfer_service.dart:456`；`docs/contracts/pairing-flow.md:383`。
- **证据与触发**：服务器对同 device_id 的新 Hello 直接关闭旧连接。FFI/daemon keeper 均把这种关闭视为可恢复断线；首个重连成功不经过失败退避。Windows GUI 与 CLI 默认使用相同的用户目录身份，也没有在所读启动路径设置同身份控制连接的跨进程互斥。task_cli::open_session 直接建立新控制连接，与契约要求 CLI 复用常驻运行时不符。
- **影响**：两个 GUI/daemon 实例共享身份时可互相踢线、重连，造成握手负载与配对不稳定；在 GUI/daemon 存活时另开 CLI task，也可能被前者自动夺回控制连接，CLI 随后无法继续等待配对。此项不要求陌生人掌握他人私钥，触发条件是同身份的合法多实例。
- **修复建议**：建立每身份单一控制连接所有者及 IPC 复用，或在启动时明确拒绝第二实例；服务器提供可区分的 replaced 原因，keeper 对替换采用明确所有权策略，不能一律立即抢回。普通断网恢复与被另一实例替换应分开处理。
- **置信度**：中高；替换及重连逻辑明确，持续争抢的具体时序未动态复现。两台设备使用不同身份的正常配对不属于此触发条件。

### R36-P2-05：claim journal 被回收后，同一请求重放先被当成码冲突

- **定位**：`crates/ak-signal/src/server_v2/store.rs:483`、`:491`、`:510`、`:520`、`:1609`；`crates/ak-signal/src/server_v2/config.rs:51`；`docs/contracts/storage-schema.md:36`、`:71`。
- **证据与影响**：journal 有 24 小时与 4096 行回收限制。journal 命中时正常幂等；被回收后，代码先检查全局 lookup_taken，已有的原码必然返回 CodeConflict。之后“同 owner+create_request_id、journal 已淘汰时重建原响应”的分支因这一顺序无法处理同 lookup 的正确重放，且配额检查也先于该回退。迟到重试不能按契约恢复原登记结果。
- **修复建议**：在新建配额和 lookup 碰撞检查前，先按 owner+create_request_id 识别既有请求，并保存/核对可重建的完整 canonical 请求与原始响应；ttl、cap、endpoint 的差异仍应拒绝。不得只移动分支后仍仅比较 lookup，也不能用当前 revision 冒充原响应版本。
- **置信度**：高；需要 journal 先发生回收，故一般几秒内的重试与现有跨版本 journal 测试不会触发。本项不是兑换额度绕过。

### R36-P2-06：wave-34 的 30 次/小时常量与同文件冻结断言直接冲突

- **定位**：`crates/ak-signal/src/server_v2/config.rs:27`、`:138`、`:152`；`crates/ak-signal/Cargo.toml:36`。
- **证据与影响**：生产常量 INVITE_CLAIMS_PER_HOUR 已为 30；default_limits_match_the_frozen_constants 仍断言该常量等于 6。在启用服务器持久化模块并执行此测试的构建中，这一断言必然不成立。历史绿色计数不能作为当前修订已通过该门槛的证据。
- **修复建议**：同步期望值，并补业务边界断言：前 30 次成功、第 31 次 RATE_LIMITED，重连和撤码不重置同 owner 的小时计数；测试配置显式覆盖 server-persistence。
- **置信度**：极高，属于直接静态矛盾；**本轮没有运行该测试，也没有报告实际测试失败次数**。

### R36-P2-07：Android 打包允许刷新失败后继续携带旧库，缺少源码版本绑定门槛

- **定位**：`scripts/package_windows.ps1:284`、`:328`、`:336`、`:379`；`scripts/build_android_ffi.sh:199`、`:218`；`flutter_app/android/app/build.gradle.kts:33`。
- **证据与影响**：-IncludeAndroid 路径中，找不到工具或构建失败仅告警，随后继续复制已有 jniLibs。MANIFEST 记录文件 hash/大小和是否 included，未记录 ffiBuilt 结果及对应 Rust 源修订。Android Gradle 直接消费暂存库，没有 Rust 源变化与重建之间的强依赖。构建脚本虽比较 target 与暂存库摘要，但冻结导出检查只含 5 个旧入口，不含任务码 ak_task_* 集合。结果是交付可显示为包含 Android，但实际携带早于 keeper/其他安全修复的库。
- **修复建议**：正式交付模式要求成功刷新，或验证来自指定修订的可追溯产物清单；记录源码提交、Cargo.lock/特性、工具链、ABI、导出集合与摘要。需要离线重用旧产物时应显式选择并把旧版本写入最终清单。检查任务码所需全部符号及 APK 实际装入的库。
- **置信度**：高（交付流程缺口）；**当前两份 `.so` 是否实际陈旧未确认**。Git 的 `37a57c0` 刷新记录支持“提交者更新过库”，不能单凭提交说明证明字节对应当前源码。本项不声称已发现 ABI 缺失。

### R36-P2-08：主入口文档与契约局部没有同步到 wave-33～35

- **定位**：`README.md:27`；`docs/HANDOFF.zh-CN.md:61`、`:66`、`:96`；`docs/ROADMAP.zh-CN.md:5`、`:13`；`docs/SECURITY.zh-CN.md:77`；`docs/contracts/pairing-flow.md:84`、`:319`、`:364`；`docs/contracts/ffi-v2.md:465`；`crates/ak-ffi/src/task.rs:477`。
- **证据与影响**：README/HANDOFF/ROADMAP 仍将 wave-29 标为进行中，HANDOFF/ROADMAP 仍称 v1 未接 CLI；SECURITY 仍写每小时 6 次 claim。pairing-flow §4a 已正确区分 Commit 竞争失败的 Pairing scope，但 §12a 仍概括为 Code scope，与实现及 §4a 不一致。§11 的建议 redeem 设备/IP 限额与实现的“每连接 10/分钟＋全局 60/分钟”未统一标明差别。ffi-v2 §8.3～8.5 已含 wave-31 修订，但未补 keeper 的保活/恢复及 C07/C12 的实现边界；keeper 字段注释仍归因为 120 秒 SESSION_TTL，真实静默断连来自 30 秒 FRAME_IO_DEADLINE。交接者可能据旧文档选择错误入口、测试配置或故障诊断方向。
- **修复建议**：按当前基线统一入口文档、版本与证据链接；明确 30/小时限制的是 claim，redeem 是另一组预算；同步 §12a 的两阶段错误 scope；补齐控制连接维护、租约、故障闩与 UI 可见性契约，并将设计建议、已实现行为和历史执行结果分开记录。
- **置信度**：高。文档中历史段落可保留，但不能继续充当当前状态；修订文档本身也不能代替 P1/P2 实现缺陷的修复。

## 三、各平台与共享层专项

### 3.1 Windows / Linux CLI

已核对 task_cli 的 code-send、code-recv、join-send、join-recv，从 RegistryClient 建连、TaskPairing、方向门禁、候选/echo、probe、consent、真实引擎到 finish。两平台共用上述 Rust 流程，因此 P1-01、P1-04、P2-01 都适用；与 GUI/daemon 共享身份还涉及 P2-04。CLI daemon 有断线 keeper，但没有等同 FFI 的 20 秒 ping，其控制连接会受相同静默期限影响并依靠重连；恢复耗尽仍有 P1-02。

`task_cli.rs:581` 安装 redeem-offer 前置门禁，验证后的回调再完成本次任务簿记；方向不互补时不会先消耗一次性额度。`--advertise` 在 `task_cli.rs:1056` 加入显式候选，仍走受限 endpoint blob、候选排序和真实 QUIC 拨号；参数本身不是路径或身份认证。`:958` 的 echo 只应答探测包、拒绝全零应答 nonce，带来源预算，未发现与 FFI 相比新增无限相互应答环。

finish 会取消接收、echo、owner-loop，尝试有界撤码并关闭控制连接，但不管理 Terminal 的阻塞 stdin 任务。现有 CLI e2e 测试源覆盖双向传输、拒绝、注入超时、方向冲突、对端断开与 TTL；不能替代真实终端或等待超过服务器 30 秒的场景。本轮没有 Windows CLI、Linux CLI 构建或进程级验证。

### 3.2 Windows GUI

真实入口使用 FFI 任务服务；`flutter_app/lib/main.dart:438` 将 facade 的 isEngineAlive 传给 AkTaskFfiBindings。`ak_engine_facade.dart:292` 在 engine_free 前撤销租约，`ffi_simple_transfer_service.dart:206` 先释放借用服务，再释放原生 owner。未找到 keeper 新增 Dart 原始句柄使用，因而没有据此新增 C07 UAF 结论。

主要未闭合点是 P1-02 的恢复、P2-02/P2-03 的故障升级与显示，以及 P2-04 的同身份多实例。C12 的单 tick 事件/进度各 64 条预算具有实现；事件原生错误与 StateError 租约撤销有停止路径，但不能据此覆盖进度异常组合或真实页面显示。

Windows CMake 的 C09 修订在 `flutter_app/windows/runner/CMakeLists.txt:80` 使用 ALL target，每次构建复制 DLL、compare_files 校验，DEPENDS 指向源 DLL，并在 `:98` 从相同来源安装；未发现原先仅 runner 重链才复制的缺口。Windows 打包脚本也有覆盖与比较 DLL 的步骤。该规则保证“复制所选 DLL”，单独运行 Flutter/CMake 并不证明所选 DLL 已由当前 Rust 源重建。本轮未执行 CMake/MSVC、Flutter Windows 构建或发布包加载。

### 3.3 Android GUI

Android 与 Windows GUI 共用 TaskStack keeper、Dart 事件泵、consent 与租约实现，P1-02、P2-02、P2-03 同样适用。Android 应用私有身份目录经 path_provider 解析；主 Manifest 含 INTERNET 权限，minSdk=24 与 Android Rust 构建目标一致。原生 MainActivity 的 SAF 目录选择、权限持久化尝试及探测文件清理有实现，但这些不是 Rust 用户文件接收已完整支持 SAF/MediaStore 的证据。

ABI 配置包含两层约束：abiFilters 只允许 arm64-v8a/x86_64，packaging.jniLibs.excludes 排除 armeabi-v7a/x86；对应两个 jniLibs 目录均有受版本控制的 libak_ffi.so。没有从源码配置发现新的 ABI 过滤/排除矛盾。`37a57c0` 与此前提交包含 Android 库刷新，但具体 source→ELF→APK 的绑定仍受 P2-07 影响；本轮未重建库、检查 APK 或执行 JNI/FFI 加载。

前后台切换、屏幕关闭/Doze、Wi-Fi↔蜂窝、Activity 销毁重建及长于重试批次的失联，都可能使定时保活无法按 20 秒执行；目前应列为尚未覆盖的动态验收，不能从桌面代码 timer 的存在推导 Android 后台长时间在线。

### 3.4 registry v2

**帧与配对契约**：protocol-v2 §10a 的受控帧层扩展、512B endpoint 上限、InviteClaim 专属 max_redeems 与未知字段/尾随字节拒绝具有对应 codec。pairing-flow §6a 与 storage schema v2 的 endpoint 快照位置相符。owner/redeemer endpoint 是未经 transcript 签名覆盖的提示，这一点已在契约明确；其内容不能作为身份凭证。

**额度与事务**：`server_v2/store.rs:970` 起的提交判定在写事务内执行，`:1038` 再次按 committed/applied/ready 计数，超限 pending 同事务转 aborted，再返回 REDEEM_EXHAUSTED；未发现把 begin 预检当作最终并发互斥的回退。Commit 竞争错误由 `engine.rs:1869` 使用 Pairing scope，§12a 的旧文字应修正。schema v1→v2 迁移及 36B/历史 41B 默认请求体等价仍有对应实现；新增发现集中在 journal 被回收后的顺序错误 P2-05。

**限速**：claim 的 30/小时由持久库按 owner 和 created_unix_ms 计数，包含撤销后的行（`store.rs:499`），不会仅因 TCP 重连或撤码重置。redeem 的 10/分钟计数属于 ConnState（`engine.rs:493`），重连会获得新连接桶；全局 60/分钟窗口（`:376`、`:1347`）仍在，故不能宣称“重连可绕过全部限速”。契约建议的每设备/每 IP 配额并未等价实现，应诚实记录当前共享全局预算的可用性边界。

**连接生命周期与 ping**：30 秒 FRAME_IO_DEADLINE 既限半帧，也限静默，因此所有长期等待客户端必须适配；P1-03 是握手槽回收问题，不是这条帧期限没有启用。Ping 只在已认证连接处理（`engine.rs:694`、`:703`），固定 nonce 的 Pong 回到同一 TLS 连接，并受每连接帧桶约束。20 秒一次的 keeper 没有引入任意目标反射通道或可见的载荷放大路径。

### 3.5 旧版 signal / STUN

旧 signal 使用独立服务路径和端口模型，未见文件上传/数据中继消息；候选与协调会话不携带用户文件内容。Hello 签名、时间窗口、帧频率、候选数、每设备 session 数都有实现，但不解决 P1-05 的接入与 I/O 生命周期问题。

STUN 只返回观测地址，`server.rs:248` 未配置应用层应答限速。该 UDP 服务和客户端 echo 的威胁模型不同于 keeper 的同 TLS 连接 Ping/Pong；本轮未测量伪造来源流量或放大比例，也不以 keeper 审查替代 STUN 流量验收。旧 Hello 的有界 replay cache 在容量压力下会淘汰尚在时间窗内的条目（`server.rs:128`）；容量压力后的重放行为仍需专门威胁验证，不能把正常重复包测试解释为无条件抗重放证明。

### 3.6 共享安全、资源与存储边界

| 模块/链路 | 本轮源码结论与边界 |
|---|---|
| ak-core / ak-net / ak-engine | Observation 明确是模型输入；真实直连使用 QUIC 对端证书/指纹与授权回调（`ak-net/src/lib.rs:492`、`:531`），禁用迁移并在开流前检查路径变化（`:721`）。本轮未发现服务器文件数据回退；私有 IP、探测成功或模型布尔值均不作为密码学直连证明 |
| ak-identity / 配对 orchestrator | 邀请秘密派生认证材料，MAC/签名绑定 transcript；未知 code_id 在取得本地秘密之前失败（`orchestrator.rs:899`），验证后的指纹才进入授权。未发现任意服务器返回公钥可直接覆盖此链的新路径 |
| TaskPairing / FFI consent | 前置方向门禁、真实指纹接收准入、同意归属与连接状态原子检查均有实现（`ak-ffi/src/task.rs:822`、`:2826`）。engine 在拒绝后、发送 Accept/创建用户文件 sink 前返回（`ak-engine/src/lib.rs:3533`）；零落盘口径应为“零用户文件内容写入”，运行时目录或元数据初始化可能更早发生 |
| owner-loop 跨重连 | `client.rs:355` 的 clear_waiters 保留 plane/pairing 订阅；`orchestrator.rs:749` 的 owner-loop 继续消费同一订阅。正常成功重连不必重建 loop；中途 Proof/Prepared/Commit 恢复仍需组合测试，不能仅凭订阅存活断言整个业务自动恢复 |
| keeper 生命周期 | `task.rs:493` 的 abort_tasks 包含 keeper，正常 teardown 在 `:613` 调用。未发现正常关闭遗漏 keeper 的永久泄漏。初始化中途停止的回滚只直接 abort owner/pump（`:1384`、`:1406`），keeper 依赖 stopped/关闭信号或 runtime 结束退出；这一细分时序缺少专项证据，不上升为已确认永久泄漏 |
| 事件与连接簿记 | 原生事件队列有容量上限，Dart pump 有 tick 预算，连接状态归并与任务完成终态有实现；C12 仍受 P2-02/P2-03 限制。控制面断开不再直接终结健康数据任务，属于符合架构的设计 |
| ak-wire / ak-storage / ak-store / scheduler | 阅读到帧资源预算、路径组件校验、分块摘要、原始字节传输、staging 隔离、检查点/队列及原子替换代码；未发现新增压缩/转码路径。普通文件内容一致与 mtime、权限、ACL、扩展属性等跨系统元数据能力应分别承诺，不能互相代替验收 |
| ak-discovery / 候选排序 | mDNS/手工候选及本地优先排序是发现提示；后续身份与路径授权仍由传输层执行。未发现将发现记录直接作为陌生发送端接收权限的新路径 |

**既有存储限制复核，不作为本轮新增发现重复计数：**

1. **符号链接 TOCTOU 仍存在。** `ak-storage/src/lib.rs:11` 明确记录残余窗口；`:1344` 检查后再 OpenOptions::open，目录路径也不是全程绑定的受限目录句柄。并发本地进程若能修改接收目录组件，仍可能在检查与使用之间重定向操作。`docs/BACKLOG.zh-CN.md:156` 的历史关闭状态仅承诺“收窄形态”，不等于竞态已消除。后续应落实平台目录句柄/no-follow 打开、提交与清理；无法保证时明确限制接收目录信任边界。本轮确认机制存在，未做竞争替换复现。
2. **Windows FAT/exFAT 的无覆盖提交保证不足。** `ak-storage/src/lib.rs:1078` 在不支持硬链接时回退 fs::rename；前置不存在检查与 rename 之间可插入其他任务创建的目标，发生覆盖。该边界已在 `docs/BACKLOG.zh-CN.md:161` 披露，与 `docs/contracts/storage-schema.md:176` 的严格门槛仍有距离。应明确拒绝该能力，或采用并验证平台无替换原语；不能把 NTFS/普通 POSIX 的 hard-link 路径成功扩大为所有卷的证明。
3. 本地 journal 的 Windows 替换降级仍记录有 remove→rename 间隙（`ak-store/src/pairing_journal.rs:442`）；历史重放/恢复用例是模型证据，不能当作断电与所有文件系统耐久性证明。终审“通过”口径必须保留这些已知限制的真实范围。

## 四、测试盲区清单（建议后续执行，本轮全部未执行）

| 优先级 | 需要覆盖的场景 | 现有源码证据缺口 / 应观察的结果 |
|---|---|---|
| P1 | CLI code-send/code-recv 发码后静默 35 秒、65 秒再由互补 join 命令加入 | 当前测试多为即时加入；必须使用生产 30 秒 reader deadline，证明连接持续在线或自动恢复，码仍能完成配对 |
| P1 | FFI keeper 连续多个 20 秒周期、服务器无其他业务流量 | 未找到明确验证跨多个静默期限的 keeper 专项断言；需验证收发 Ping/Pong、owner-loop 仍回答新 Offer，而非只看 watcher 状态 |
| P1 | registry 下线超过全部重连尝试，再恢复 | 现有 reconnect 测试主要显式调用后成功；需证明无需用户重建 runtime 就继续尝试，且不会停在 Connecting |
| P1 | 已连接 TCP 黑洞、ping 连续超时、网络恢复 | 区分 EOF/I/O error 与既无 EOF 也无 Pong；验证应用层探活触发恢复、退避有界、旧代际不会误伤新连接 |
| P1 | registry TLS 失败、超时、ALPN 错误累计及超过连接上限 | 需要在每个失败出口断言配额归还；失败批次后合法客户端仍能登录；过载任务数、关闭回收也必须有界 |
| P1 | CLI Terminal stdin EOF、I/O 错误、读取任务错误、显式 Enter/y/n | Scripted 与 WindowTimeout 无法覆盖；EOF/错误不得同意，拒绝后接收目录不得出现用户文件内容 |
| P1 | 旧 signal 的无 Hello/半帧静默、慢读饱和、关闭 | 现有快速畸形帧测试不等同资源耗尽测试；验证接入上限、服务仍可接受健康客户端、sweep/shutdown 不被单端阻塞、无遗留 writer |
| P2 | 跨重连的 owner-loop：发码后、收到 Offer 后、Proof/Prepared/Commit/Applied 各阶段 | `client_v2.rs` 有订阅跨重连测试，TaskPairing 有“控制断线不结束数据任务”测试，但需将 keeper 与真实 owner-loop、endpoint 缓存、pending 去重及 journal 恢复组合验证 |
| P2 | 同一身份两个 GUI/daemon、GUI/daemon 加 CLI task | 验证唯一所有者/复用策略，观察替换原因与握手频率，防止互踢、反复重连和 CLI 永久掉线 |
| P2 | consent 超时后第二次 Offer、迟到输入、正常任务结束 | 使用真实或可控终端输入源，确认旧读者不抢答案、输入任务数有界、正常退出不等待无限阻塞读 |
| P2 | C12：event poll 正常，progress poll 连续 AkFfiException | 现有事件失败/坏 JSON 测试不覆盖；达到阈值必须停止，并且一条完全健康 tick 才清零 |
| P2 | C12 的真实 adapter→首页/详情页故障显示与等待中断 | 不能只断言内部 phase/failureReason；需要看到页面错误、后续操作阻止、接收等待即时结束及明确恢复入口 |
| P2 | keeper 与 teardown/C07：睡眠、ping、重连、初始化回滚各暂停点 | 正常 abort_tasks 已覆盖 keeper，但缺少这些交错时序证据；确认无持续后台连接、悬挂请求或撤销租约后的 native 调用 |
| P2 | claim 30/小时的第 30/31 次、撤码后、重连后、服务重启后 | 修正 6 的静态断言后，还需验证持久 owner 预算；不能只测默认常量值 |
| P2 | journal 超 24 小时/4096 行后，原 claim 相同及不同内容重放 | 检查相同请求返回原响应、变更 ttl/cap/endpoint 返回 REQUEST_CONFLICT；保留 tombstone，不重开旧码 |
| P2 | Android source→两 ABI ELF→最终 APK 与 Windows source→DLL→runner | 覆盖源码修订/特性/导出集合/摘要，刷新失败不得误标最新；检查 APK 中确实没有不支持 ABI 的残留目录。仅比较 target 与拷贝不证明源码来源 |
| P2 | Android 前后台/Doze/网络切换后等待码继续可用 | 20 秒 Rust timer 不等于系统允许后台运行；验收配对恢复、旧候选重新探测、路径变化重新授权，并区分模拟器与物理设备 |
| 已知限制复核 | 可写父目录的符号链接竞态、Windows 无硬链接卷并发提交、journal 替换中断 | 静态链接拒绝及普通 NTFS 传输不覆盖这些问题；未闭合之前，保持文档中的边界，不扩张为无竞态/全卷原子/断电耐久保证 |

上述清单是后续修复与验收建议，不表示获得了新的执行证据。Python 仓库检查、Rust 测试与静态检查、Dart/Flutter 测试、Windows/Linux/Android 实际构建及 release 深测，本轮均按只读评审约束未执行。

本报告的最终计数为 **P0 0 / P1 5 / P2 8，总计 13 项**；既有存储限制与尚待威胁验证的边界单列，未重复计入。报告不改变 ADR-001：任何修复都必须维持仅直连的文件数据通道，无法直连时等待/暂停/重试。
