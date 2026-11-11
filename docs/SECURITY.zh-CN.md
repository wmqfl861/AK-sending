# 安全模型与发布门槛

保护目标：原文件完整性、发送/接收授权、设备私钥、目标目录边界，以及无服务器数据传输约束。
威胁包括恶意同网设备、仿冒信令、被入侵协调服务器、畸形文件清单、路径穿越、恶意文件、资源耗尽和依赖自动中继。

## 身份和加密

使用成熟端到端认证加密协议。配对绑定持久设备公钥；二维码/可核对指纹具有用户可验证来源。
短数字码必须使用成熟安全配对机制，不能直接当内容密钥；不自研密码算法。
协调服务不得用未核验的新公钥替换已配对对端；密钥轮换需本地明确流程。
私钥保存在系统安全存储；撤销信任阻断新任务，活动任务按明示策略取消。接收可执行文件不自动运行。

## 控制面隔离

固定消息白名单、未知字段拒绝、短期会话绑定、重放保护、速率与资源限制。
无上传 API，无服务器到任意用户地址的通用代理。服务端日志不记文件名、原文、私钥、配对秘密。
普通运行日志记录错误类型及脱敏指标；真实 IP/pcap 为敏感诊断资料，需单独授权、有限保存，不提交公共仓库。
不以客户端能填写 `authenticated=true` 为安全证明。当前领域模型必须由未来可信适配器提供真实证据。

## 文件系统和资源

验证目录边界、链接竞态、重名策略、偏移长度和整数溢出；有限内存和并发；不信任远端宣称的文件大小用于无限分配。
原文件传输不保证文件本身无恶意内容；保留系统安全机制，不自动执行/绕过系统检测。

## 已实现的安全机制（截至 2026-11-03）

以下机制已落在代码并有测试锁定（执行证据见 [dev-validation-2026-11-02](../reports/dev-validation-2026-11-02.txt)，含第三轮与第六/七轮验证节）：

- Ed25519 设备密钥与指纹（`ak-identity`）：公钥即设备身份，指纹＝公钥 BLAKE3 摘要前 32 字节；配对码由指纹派生（`123-456` 形式）供人工核对；私钥种子 32 字节原子落盘，不进入任何协调面消息。
- 自签证书钉扎（`ak-identity::tls`＋`ak-net`）：DER 解析为最小结构化子集，实际规则为——TLV 长度拒绝前导零与越界；证书顶层必须恰为 TBS SEQUENCE／算法 SEQUENCE／64 字节签名 BIT STRING 三元素、无尾随元素；SPKI 以"Ed25519 OID＋32 字节 BIT STRING"结构匹配提取（issuer/subject/validity/extensions 无法遮蔽）；提取公钥与 pin 常量时间比较（自实现，无 `subtle` 依赖），`verify_strict` 校验自签名。这不是完整 X.509 模式校验（有效期等字段不检查，见已知限制）；第十七波 F4 正在收敛解析规则与文档口径（修复进行中，落地证据由集成者补）。rustls 验证器无 webpki 根、无主机名校验，端点级默认客户端配置拒绝一切对端，仅 `connect(peer, pinned)` 可建立通道，握手后还会从已建立连接重导出指纹复核。
- 数据面加密：QUIC（quinn）＋rustls（ring 后端），QUIC 握手按规范要求 TLS 1.3，ALPN 为 `ak/1`；一个 UDP socket 只对一个对端地址，无回退传输、无服务器数据路径。
- 钉扎失败即断（`ak-net`、`ak-engine`）：证书不匹配、传输错误一律中断且不降级，`wrong_pin_fails_closed_before_any_transfer` 等测试锁定。
- 路径越界与符号链接防护（`ak-core::manifest`＋`ak-storage`）：拒绝 `..`、绝对路径、盘符与 Windows 不友好组件；已存在路径逐级 `symlink_metadata` 复查（符号链接目录无法靠 `exists()` 混入）；根本身为符号链接即拒绝；手工构造的清单条目同样复查。
- 原子提交（`ak-storage`）：`.akpart` 临时文件逐块摘要校验，整文件摘要＋fsync＋改名后才出现最终名；冲突策略仅报错/改名，不静默覆盖——当前由"提交前存在性检查＋改名"实现（Windows 上改名到已存在名天然失败兜底；POSIX rename 会覆盖已有文件，仅靠检查前置拦截，检查与改名之间存在竞态窗口）。第十七波 F3 正在升级为平台原子 no-replace（Windows 原子语义＋POSIX link 方案），使"绝不静默覆盖"从策略约定成为原子保证（修复进行中，落地证据由集成者补）。
- 检查点顺序规则（`ak-store`＋`ak-engine`）：完成标记严格晚于最终校验与保存提交（引擎在发送 FinalAck 前写完成）；检查点文件本身以临时文件＋fsync＋改名原子写入，并发写不留残余临时文件。
- 信令封闭消息集与限频（`ak-signal`）：消息集为 `Hello..=ServerError` 封闭枚举，无 payload/blob 字段，帧上限 4096 字节，候选仅 `SocketAddr` 且至多 16 个；Hello 须带 Ed25519 签名且新鲜度窗口 ±10 秒；令牌桶限频 32 帧/秒（突发 64）、会话 TTL 120 秒、每设备至多 4 会话，超限或不可解码即断开，均由测试锁定；STUN 为内置 RFC 5389 子集。
- 禁依赖扫描（`scripts/check_repository.py`）：解析 Cargo.lock 禁止 relay/TURN/WebRTC/iroh 类 crate 进入依赖图，2026-11-02 扫描结果 clean。
- 连接阶段地址族快速失败与空闲预算（`ak-engine`，2026-11-03）：`connect_bounded` 在发起 QUIC 前校验本地 socket 与对端地址族，v4 拨 v6（或反之）立即以 InvalidInput 失败，不再无限悬挂于 QUIC 重试（该缺陷由 IPv6 场景测试发现，回归测试 `connect_family_mismatch_fails_fast_instead_of_hanging` 钉死）；连接等待同时受空闲预算约束，不可达对端最终以错误/超时暴露。防挂起（资源耗尽向量）与防地址族错配。
- 存储满预检（`ak-engine`，2026-11-03）：`check_headroom` 纯决策函数在接收端应答 ResumeRequest 前比较目的目录报告的剩余空间与任务总字节，不足即以 `EngineError::StorageFull`（携带真实余量/需求字节）中止并经协议 `ErrorCode::StorageFull` 告知对端；余量未知（Windows `available_bytes` 返回 None）放行，耗尽仍由真实写错误暴露——预检拒绝浪费，写错误兜底，不伪造成功。
- 并发接收的失败隔离（`ak-engine`，2026-11-03）：`serve_concurrently` 一次 bind、每个被接受连接一个独立任务，逐任务运行与单任务接收完全一致的协议体（授权回调与策略闸门按连接执行）；检查点互不重叠——每任务独占自己的 `<job>.akjob` 文件（原子写），接收目录仅经只读 `LocalSaveTarget` 共享（`prepare_file` 无内部可变状态，不同条目映射不同 `.akpart` 文件）。但记录键当前仅按内容派生的 job id 生成（2026-11-04 审核第 4 条确认）：同一源并发发给多个对端、或同机收发共享 checkpoint 目录时，仍会共享并互相覆盖同一 `<job>.akjob` 记录；第十七波 F2 正在引入 record_id 分离（记录键纳入方向/对端/目标身份，修复进行中，落地证据由集成者补）。任一任务失败只填充自己的结果槽位，不拖垮其他任务（真实双发送端冒烟 2/2 字节一致）；`ConflictPolicy::Fail` 下并发任务竞争同目标路径按文件粒度失败，与两次顺序接收语义一致；accept 握手在独立任务中驱动，不会被驱动循环的选择分支取消中途握手。同名 `.akpart` 的文件系统级竞态语义未消除，如实保留为已知限制（见“已知安全限制”节）。
- 两层控制字符注入防护（`ak-signal::codec`＋`ak-wire`，2026-11-03）：第七轮发现并修复 Hello 设备名控制字符注入缺陷。协调面字符串字段（设备名/错误文本）与传输面显示字符串（相对路径/根名/错误消息）统一拒绝 C0 控制字符（U+0000..U+001F，含 NUL）、DEL（U+007F）与 C1 控制字符（U+0080..U+009F）：NUL 截断与终端转义序列无法借名字进入 UI 与日志；信令侧同一条规则分别落在解码路径与内存构造路径（`get_str`/`verify_hello` 共享 `is_control_free`），两条路径不会漂移；overlong UTF-8 编码先被 UTF-8 校验拒绝；`rejects_control_characters_in_display_strings` 等测试锁定。
- 对抗性输入加固（`ak-signal`，2026-11-03）：三项实测存活并有测试锁定——500 帧原始 TCP 帧模糊（四种形态对抗帧混合、多次重连，模糊进行中合法客户端仍完成 Hello/Welcome 握手，模糊结束后服务器继续正常服务）；500 个畸形 UDP 数据报 STUN 洪泛（穿插的 50 个合法 Binding 请求全部获得事务匹配的正确响应，洪泛结束后仍存活）；限频/会话上限旁路（单条连接向 5 个不同目标连发 ConnectRequest，第 5 个命中每设备会话上限被拒且重复请求拒绝稳定，断开连接即释放配额）。
- 长文件名 part 回退命名（`ak-storage`，2026-11-03）：极端名场景测试发现 249 字节文件名附加 `.akpart` 后缀超出 NTFS 255 字节组件上限的真实缺陷；修复为确定性回退——最终组件过长时改用同目录 `.akpart-<最终组件 BLAKE3 摘要前 16 字节 hex>` 短名，防止组件超限导致传输失败；回退名仅由最终名派生，断点续传仍能找到同一 part 文件；孤儿清理识别 `.akpart` 结尾与 `.akpart-` 开头两种 part 形态；边界测试翻转为完成路径。

## 已知安全限制（截至 2026-11-03）

与上节并存的事实性限制，修复前不得在对外表述中省略：

- probe nonce 非密码学随机（`ak-net`）：以纳秒时钟＋计数器组合生成；探测包不承载认证主张，仅用于打洞配对。
- classify 的 VPN 启发式局限（`ak-net`）：VPN 也可能使用私网地址段，路径分类只是路由提示，不构成"本地即可信"判定；IPv6 前缀按 /64 简化匹配。
- part 文件 TOCTOU 残余窗口（`ak-storage`）：`.akpart` 路径的符号链接检查与随后的打开/改名不是单一原子操作，两者之间仍存在被竞争替换的理论窗口；逐级 `symlink_metadata` 复查覆盖已存在组件，但不承诺消除全部竞态。
- 无证书有效期策略（`ak-identity::tls`）：自签证书约十年有效期，钉扎验证不检查有效期字段（过期或尚未生效不会导致失败）。
- 协调面为明文 TCP＋签名认证、无 TLS（`ak-signal`）：Ed25519 Hello 签名证明设备密钥持有者并限重放（±10 秒），但不加密信令内容；候选地址、在线状态对链路观察者可见。
- 引擎限制（`ak-engine` 文档）：接收端每次 `receive()` 仅接受一个任务（有意为之、已文档化；带内 Pause/Resume 与空闲超时已于第二轮实现，能力协商生效）；并发接收 `serve_concurrently` 保持同一语义——每连接一个任务。
- 私钥暂以用户目录下 32 字节种子文件保存（原子写入），尚未接入系统安全存储（Keychain/Keystore 等）。
- 同出口提示存在误报场景（`ak-signal`，2026-11-03）：`PeerSignal::SameEgress` 仅比较两会话端在协调面的 TCP 对端 IP 是否一致——两端经同一反向代理统一来源 IP，或 CGNAT 后不同设备撞同一公网 IP 时会误报"同出口"。该提示只影响候选排序（LAN 优先），不作认证或信任判定；消费方不得将其当作"同一可信内网"的证据。
- 多根同名仅精确匹配（`ak-engine`，2026-11-03）：多根合并发送的根名查重为精确字节比较（排序去重），大小写不敏感卷（Windows/macOS 默认）上 `Photos` 与 `photos` 不会被拒绝，两者可在接收端合并或触发冲突策略；发送端不保证根名的大小写语义唯一。
- socket2 `only_v6` 在 Windows 的读取缺陷（`ak-net`，2026-11-03）：`IPV6_V6ONLY` 无可移植的查询方式——部分 Windows 版本 `getsockopt` 返回 1 字节 BOOLEAN，socket2 0.5 的 `only_v6` 读取结果不可靠。已规避：仅 `bind_dual_stack`（构造时显式清 V6ONLY）声称双栈身份，`bind_with_socket` 收养的 socket 一律报告非双栈，`connect` 的 v4→映射拨号改按 socket 地址族判定而非该标志。
- 双栈映射流量的间歇 WSAENETUNREACH（`ak-net`，2026-11-03）：开发中观察到 Windows insider 版本对 `::ffff:` 映射发送间歇性返回 `WSAENETUNREACH` 且可持续数分钟，即使 socket 本为双栈；双栈端点仅扩大候选集，不替代探测/引擎层按地址族的可达性验证，`ak-net` 内双栈测试因此为尽力型（best-effort，环境相关现象不作断言门槛）。
- `serve_concurrently` 无停止通道（`ak-engine`，2026-11-03）：完成条件即 `expected_jobs` 个连接全部接受并结束，没有中途停止参数；期待慢发送方的调用方应调高 `ReceiveSpec::idle_timeout`（约束每次 accept 等待），未填满的槽位以 `EngineError::IdleTimeout` 报告而非永久悬挂。
- 存活连接迁移与路径重授权（`ak-engine`/`ak-net`，2026-11-04，INV-004）：准入与策略闸门只在连接建立时执行一次，不随存活连接的路径变化重评；重连场景（断线→新连接）已重新走钉扎＋闸门并由 `t08_reconnect_recheck` 锁定，但该测试明确不覆盖 connection migration。第十七波 F5 正在禁用 QUIC active migration（修复进行中，落地前按未禁用对待，落地证据由集成者补）；禁用后存活连接不再静默迁移对端地址，而连接迁移后的重授权机制（迁移重协商准入/路径）为后续项。

## 邀请码配对安全机制（2026-11-04，第十八至二十一波）

决策与契约见 [ADR-004](adr/004-invite-pairing.zh-CN.md) 与 `docs/contracts/`（pairing-flow/protocol-v2/storage-schema）；以下机制均已落地并有测试锁定（执行证据见 [dev-validation-2026-11-02](../reports/dev-validation-2026-11-02.txt)"第十八/十九波"至"第二十一波"节）。

### 码格式与生成

- 160 bit 熵：客户端以操作系统 CSPRNG（Rust `OsRng`）生成 20 字节随机，Crockford Base32 编码为 `AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX` 展示格式（`AKI1` 为格式版本前缀，不计入熵）；不使用 `ak-signal::util::unique_bytes`（时钟与计数器派生工具，非密码学随机源）。解析规范化仅接受 ASCII 小写折叠与规定位置的连字符（或无分组），拒绝 Unicode 同形字符、控制字符、任意空白、错误长度与未定义符号。邀请码是 bearer credential：码泄露后持有者即具配对能力，不承诺区分"原本想邀请的人"与"窃取了码的人"。
- 域分离双派生：`lookup_digest` 与 `auth_key` 以不同 BLAKE3 derive-key 标签（"AK invite lookup v1"/"AK invite authentication v1"）从同一 20 字节秘密派生。服务端只接收 `lookup_digest`（仅能用于受限查找，不能当配对凭证）；`auth_key` 只留在两个端点，用于配对 transcript 的 MAC。
- 服务端不存明文：权威库 `invite_codes` 表只有 `lookup_digest`，没有也不得增加明文码、`auth_key` 或 MAC 材料；`InviteCode` 的 `Debug` 手写为掩码提示，`invite list` 不输出明文码；日志/诊断不记录邀请码、lookup 或 MAC。
- 幂等与唯一性：claim 线性化点是唯一索引保护下的 INSERT 事务提交（SQLite `BEGIN IMMEDIATE`），同码并发 claim 最多一个成功、其余 `CODE_CONFLICT`；同 owner 同 `request_id` 同请求内容返回原结果、不同内容拒绝 `REQUEST_CONFLICT`（有界幂等记录，库表唯一约束承载）；网络超时先以原 `request_id` 重试，不直接生成新码。随机性防猜测，唯一性由权威事务保证。

### 反枚举与防暴力

- ghost-pending 反枚举：兑换不存在/错误/过期/撤销的码，服务端先返回与真码相同字段形状的 `InviteRedeemPending` 并在内存登记 ghost，到期限统一返回 `PAIR_UNAVAILABLE`——兑换方在等待期与失败期均无法区分"码不存在"与"码存在但不可用"（无存在性预言机）；配额拒绝在 lookup 查询前执行；owner 查询自己的列表可见真实状态。（终审更正 2026-11-05：上述统一仅覆盖兑换首响应，"无存在性预言机"以此收窄——status 查询与终态响应存在可区分性（终审发现，修复中，见 [dev-validation-2026-11-02](../reports/dev-validation-2026-11-02.txt)"终审更正"节第 1 条）。）
- 防暴力限速（四层桶，2026-11-11 CR-P1-03 修订）：redeem **每连接 10 次/分钟＋每来源 30 次/分钟＋每设备身份 30 次/分钟＋全局熔断 600 次/分钟**滑动窗口（替换原"每连接 10＋全局 60"双桶——旧全局 60 次/分钟可被单一来源用约 6 个自生成身份打满，使**其他来源**的全部合法兑换被 `RATE_LIMITED`，一台普通机器即可令所有新任务无法建立）。来源按 IPv4 /32、IPv6 /64 归一；每来源与每身份窗口为服务端共享表，跨连接、跨重连计数（重连只获得新的每连接桶，不能重置其余三层），表项带 TTL 空闲回收与容量上限；所有尝试无论成败一律计数，不设"查无此码"单独惩罚窗（码 160 bit 熵，在线枚举不可行，预算限制总工作量而非惩罚猜测）；每身份窗口之上永远叠加每来源窗口，批量生成身份无法放大单一来源预算。叠加 owner 维度预算（每 owner 32 个 active 码、**claim 30 次/小时**（2026-11-09 wave-34 `8e5ba7e` 从 6 次/小时上调：6/时对正常使用过紧；按 owner 持久计数、含已撤销的行，重连与撤码不重置）、32 个并发 pending 兑换）与全局 pending 4096 上限；既有帧限频（32 帧/秒、突发 64）保留。**claim 与 redeem 是两组独立预算**：30 次/小时限的是 claim（码登记），redeem 走上面的四层桶。设备密钥可批量生成，设备级限速不单独承担防暴力，必须叠加来源与全局资源预算。
- 按来源连接准入（2026-11-11 CR-P1-03）：v2 服务端在 accept 时按来源（IPv4 /32、IPv6 /64）限制**并发连接 8、其中握手中 4**，超限直接断开该 socket、不 spawn 任务——只建 TCP 不完成 TLS 握手的连接最多占住单一来源自己的配额，无法再占满全局名额拖垮其他来源；TLS 握手期限 10 秒（从 30 秒缩短），首帧期限 15 秒；来源计数在连接结束（含握手失败/超时/中止）即释放，空闲表项带 TTL 回收，无泄漏（R36-P1-03 的 RAII 释放语义沿用并扩展到来源维度）。
- 防中间人（换钥/换码/nonce 检测）：256 字节 `PairTranscript` 绑定双方设备公钥、`code_id`/`lookup_digest`/`code_revision`、双 nonce 与期限；owner/redeemer 各自构造 `MAC(auth_key, 角色域 || transcript)`＋Ed25519 签名（覆盖 `角色域 || transcript || mac`），MAC 验证恒定时间。被入侵的协调服务替换任一侧公钥→签名验证失败（换钥检测）；交换、替换或重放其他码的 transcript/证明→MAC 验证失败（换码检测，`auth_key` 仅由码秘密派生）；nonce 重放与过期证明为独立错误变体拒绝；对已有信任，服务端返回新公钥不能静默替换本地簿记录。仅有 TLS 不足以防被入侵协调服务替换首次配对公钥，故 TLS 之上叠加该双端证明。
- TLS-only 不降级：控制协议 v2 仅经 TLS 1.3（ALPN `ak-signal/2`）使用；服务端证书公钥即 `RegistryId`，客户端显式 pin（`--registry-fp`，持久化于身份目录 `registry.pin`），不自动接受未知 pin、不降级现有明文 TCP（v1 明文信令保留独立端口，两者无共享 socket、无降级路径）；Hello v2 签名绑定 `"AKHELLO2" || registry_id || TLS exporter || Hello 规范字段（不含 signature）`；`AuthChallenge` 只在当前 TLS 连接上使用一次、最长有效 10 秒。

### 已知限制（第十八至二十一波如实声明）

- ghost 兑换记录仅存内存（`server_v2`）：服务端重启后 ghost 丢失，不存在码与存在码的失败时序在重启窗口可能重新可区分；真实挂起事务本身持久化并支持重启恢复，但 ghost 形态的时序均匀性不跨重启承诺（不声称完全消除所有时序侧信道）。
- owner 握手窗口崩溃不可恢复：owner 必须在线自动应答兑换；在事务到达 Prepared/Commit 之前 owner 进程崩溃或离线，该次事务按期限失败（redeemer 统一看到 `PAIR_UNAVAILABLE`），恢复路径只续接服务端已 Prepared/Commit/Ready 的事务，半途握手的 owner 侧不能续接，需重新发起兑换。
- 波二十并行产物 `ak-signal::client_v2` 与 `server_v2` 曾在三处握手常量（ALPN/pin 语义/exporter label）上分歧、从未互跑；第二十一波 `ak-session::pairing::RegistryClient` 按服务端公共面构建绕过（差异记录于其模块文档）。第二十二波（2026-11-04）已将 client_v2 三处常量对齐 server_v2（ALPN `ak-signal/2`、exporter label、pin＝原始 32B 公钥），本条历史保留。
- 恶意 registry 恢复攻击面（终审 P1-1，2026-11-05，修复中）：新任务准入链路（wire 准入三消息＋引擎 on-channel 入口）未绑定认证材料——`TaskOpen` 缺 `pairing_id`/方向/attempt/`admission_nonce`（`group` 32 字节与冻结契约 `code_id` 16 字节不一致），恢复/重试路径同样无 pairing_id/attempt/nonce/lease 可核；`AdmissionCtx` 字段公开可构造、`AdmissionDecider` 仅见 `&[u8;32]` group、引擎解构 `TaskOpen` 时丢弃 `group_registry`；接收端对未宣告 `CAP_SCHEDULER` 的对端回退旧 Offer 流程（decider 完全不执行，非 fail-closed，与 ADR-004"不支持则 PROTOCOL_UNSUPPORTED"冲突）；`send_on_channel` 未校验通道对端指纹==`spec.pinned`。据此，恶意 registry/对端可借恢复或 capability 降级路径取得未授权任务准入（新链路内）。终审核实生产 CLI/FFI 未接该入口（CLI daemon 调度开关走 Fake executor、FFI 未完成双端 wire admission），故不构成当前 CLI 配对簿 TLS 身份校验的远程绕过——按终审口径归类为"新准入安全契约未落地、公开接口不可安全接线"，修复（受控 `AuthorizedTaskContext` 绑定真实通道身份/Ready relation/registry/code/pairing/nonce/permit，接收 fail-closed）完成前不得接入生产 executor；逐项见 [BACKLOG](BACKLOG.zh-CN.md)"终审发现（2026-11-05）"节。

## FFI 边界（2026-11-03 契约冻结；实现已落地经 C ABI 测试；2026-11-04 第十七波修复带来契约增补、落地中）

`crates/ak-ffi` 以句柄式 C ABI 向 UI 暴露引擎（决策与备选方案见 [ADR-003](adr/003-ffi-boundary.zh-CN.md)）。本节登记边界防护；实现已于第十至十二轮落地并由 C ABI 端到端测试锁定（版本/环回传输/坏 spec 反例/取消/双引擎/并发接收，见 [dev-validation-2026-11-02](../reports/dev-validation-2026-11-02.txt) 相应节）。第十七波修复代理带来契约增补（receive spec `expect` 必填、摘要算法未知显式拒绝、任务/检查点身份分离、冲突原子 no-replace、禁用 QUIC active migration、spec 边界校验收窄——逐项状态见 ADR-003"修复波落地中的契约增补"节，均标注修复波落地中），落地前实际语义以 `crates/ak-ffi` 当前实现为准。

### 已设计防护（契约层）

- 指针入参校验：跨边界字符串入参要求非空指针且为合法 UTF-8，违反即以错误码拒绝，不进入引擎；spec JSON 解析忽略未知字段（向前兼容），新增字段不导致旧调用方失败。
- 缓冲区容量协议：出参字符串写入调用方提供并声明容量的缓冲区，写入不越过声明容量，容量不足按协议显式报告（截断可见、可按需重试）而非越界；除显式释放 API 外不把 Rust 侧分配交给调用方 free，规避跨分配器释放。
- 控制字符拒绝：跨边界显示字符串延续仓库两层注入防护（`ak-signal::codec`＋`ak-wire`，2026-11-03）——C0 控制字符（含 NUL）、DEL、C1 控制字符不得经设备名、路径、错误消息进入 UI 渲染；NUL 截断与终端转义序列无法借 FFI 边界绕过既有拒绝规则。
- 句柄生命周期：句柄由 `ak_engine_new` 创建、内嵌 tokio 运行时随句柄存活，由显式 free API 释放；free 后使用与重复 free（double-free/use-after-free）属调用方契约违例，按 C ABI 语义为未定义行为（UB），不以"必然返回错误"作承诺——恰一次 free、free 后不再触碰句柄是调用方义务，各导出函数的 Rustdoc `# Safety` 节（`crates/ak-ffi/src/shim.rs`）逐条登记；边界处若有防御性检测，仅为尽力而为的诊断辅助，不构成可依赖的保证。
- unsafe 收敛：`unsafe` 仅存在于 `ak-ffi` 的 shim 且逐块带 SAFETY 注释（仓库唯一例外，其余 crate 全部 `#![forbid(unsafe_code)]`），业务逻辑全部位于安全函数中，内存安全审计面集中在一层。

### 已知限制（截至 2026-11-03，实现验收后固化）

- 调用方缓冲区协议的误用面：容量虚报（实际缓冲区小于声明容量）无法被 Rust 侧探测，属未定义行为范畴；并发读写同一出参缓冲区造成的截断/竞态依赖调用方自律，契约只能规定"缓冲区在调用期间由调用线程独占使用"的约定，无法强制。
- 跨线程句柄使用约定待固化：句柄内嵌 tokio 运行时，从哪些线程调用、能否跨线程传递/释放（Send/Sync 等价语义）需在实现验收后以测试锁定，届时在本节固化；当前仅按契约草案描述，不构成可依赖的保证。

## 无中继发布门槛

审查依赖默认配置与环境代理；配置禁止 relay 不足以单独证明无中继。
有真实双端/服务器抓包、所有控制入口的可接受消息验证、断直连故障注入、路径变化验证后，才允许声明“测试矩阵中未出现服务器文件传输”。
不能将“服务器看不见文件内容”表述成“服务器不知道通信 IP 和时间”。
跨设备冒烟证据分类（INV-008 严格口径，2026-11-04）：反向跨设备冒烟（宿主→Android 模拟器）因模拟器 slirp 回程源端口改写触发 quinn 地址校验丢弃，改经本地 Python UDP 中继垫片保四元组完成——垫片转发的是含密文的数据报（不解密、不落盘、不看明文），但它构成数据报转发路径：该证据只证明跨网络栈的 QUIC 编解码与端到端加密连通，明确排除出"纯直连"验收声明；正向（Android→宿主，10.0.2.2 slirp 直达）未用垫片，不在此列。真机双端直连与真实 NAT 验收未执行，"纯直连"验收以未含任何转发路径的证据为准。
真机双端/真实 NAT 验收矩阵补齐前，本项目不声称生产安全保证（当前验证覆盖环回、本机网卡与模拟器↔宿主跨网络栈，真机双端未测）。
