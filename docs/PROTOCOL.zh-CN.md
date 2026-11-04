# 协议 v0.1（线缆格式 wire v1 已冻结；控制协议 v2 已冻结并实现；未上线）

## 控制面

服务器只接受固定消息类型：设备在线/离线、配对协调、连接请求、候选地址、候选完成、取消连接。
字段采用版本号、固定长度设备/会话 ID、有效期、受限候选地址列表、固定长度签名及必要握手参数。
无 `payload`、`blob`、`file`、路径、文件名、目录清单、内容摘要或任意附件字段；未知字段拒绝。
候选仅接受合法端点地址与端口；不允许让服务端请求任意 URL、扫描指定网段或访问云元数据地址。
服务端不建立任意对外转发连接，也不暴露 CONNECT/通用 WebSocket 转发。
认证、会话绑定、短有效期、重放保护、候选数量和消息频率有独立资源限额，不等于文件额度。
消息字节上限已在实现冻结（信令帧上限 4096 字节、候选地址至多 16 个、Hello 新鲜度 ±10 秒，见 [SECURITY](SECURITY.zh-CN.md)），不得用巨大的通用字符串字段代替结构化协议。

## 控制协议 v2（2026-11-04 第十八波冻结，第十九至二十一波实现）

邀请码配对功能使用独立的控制协议 v2（决策见 [ADR-004](adr/004-invite-pairing.zh-CN.md)；逐消息字段、payload 预算、方向与状态规则冻结于 [contracts/protocol-v2.md](contracts/protocol-v2.md)，本文不重复数字，以该契约为准）。

- **帧格式**：独立 `codec::v2::CtrlMessage`；帧头仍 4 字节、帧版本 `2`，全帧上限 4096 字节（契约中的预算为 payload 字节数，不含帧头）；整数大端、固定数组不带长度前缀；未定义 tag、非法方向、非法状态、尾随字节全部拒绝；先验证类型与长度预算再分配，生产编码走可返回错误的 `try_encode`。
- **消息**：30 个变体（tag `0x01`–`0x1E`）——既有流程的 v2 布局（Hello/Welcome/Register/ConnectRequest/SessionOpened/Signal/Cancel/ServerError/AuthChallenge）＋邀请码管理（InviteClaim/InviteClaimed/InviteList/InviteListPage/InviteRevoke/InviteRevoked）＋配对 transcript 与双侧落簿（InviteRedeem/InviteRedeemPending/InviteRedeemOffered、PairChallenge/PairProof/PairPrepared/PairCommit/PairApplied/PairReady、PairStatusQuery/PairStatusReply、PairAbort、PairError、Ping/Pong）。`PairTranscript` 固定 256 字节、`PairProofData` 固定 96 字节；`scope`/`state`/`reason` 均为封闭枚举；不向 `PeerSignal` 增加邀请码或通用认证载荷，也不增加 `extra_json`/`metadata` 类扩展口。
- **TLS-only，不降级**：v2 仅经 TLS 1.3 使用（ALPN `ak-signal/2`；服务端证书公钥即 `RegistryId`，客户端显式 pin），不自动降级明文，也不自动接受未知 pin；Hello v2 签名绑定 TLS exporter，`AuthChallenge` 单连接单次、最长 10 秒。邀请码业务错误走固定 `PairError`（`0x2101`–`0x2108`，含 `retry_after_ms`），不回显用户输入。
- **与 v1 共存**：保留 v1 解码器及其原始布局，扩展字段不追加到 v1 帧；v1 明文信令与 v2 TLS 各用独立端口（v1 默认 7100/7101，v2 默认 7200），无共享 socket、无降级路径。已实现于 `crates/ak-signal`（`protocol_v2` codec 47 项测试、`server_v2`/`client_v2` 与 `ak-session::pairing` 真 TLS 配对 e2e）。

## 端到端文件协议

只有真实设备直连建立并认证后，双方协商文件协议版本和能力。
目标消息：Hello、Offer、Accept/Reject、ManifestPage、Chunk、ChunkAck、ResumeRequest、Pause/Cancel、Finalize、FinalAck、Error。
Offer 包含任务标识；ManifestPage 包含普通文件/目录/空目录条目、相对路径、文件大小、版本和摘要算法标识。
摘要算法标识必须为双方已知的枚举值（当前仅 BLAKE3）：未知 `digest_alg` 显式拒绝，不得静默按默认算法处理（第十七波 F1 修复落地中——2026-11-04 审核确认现状为静默改按 BLAKE3）。
能力协商新增调度准入（2026-11-04 第二十波实现）：双方 Hello 均携带 `CAP_SCHEDULER`（0x0008）后使用封闭的 TaskOpen/TaskAdmission/TaskAdmissionAck 三消息（wire 类型 13–15，仅经设备直连、不经信令）完成双端任务准入（许可/排队/拒绝＋有界 lease 与重试提示）；不支持该能力的旧端对新邀请码任务返回 `PROTOCOL_UNSUPPORTED`，不退回无分组准入路径。接收端 staging 同步改为每任务命名空间（`<目标根>/.ak-staging/<task>/`，第二十波）。
文件内容和文件元数据均留在端对端通道。失败状态摘要可用于控制面运维，但不得携带文件路径/内容。

文件大小/偏移为 64 位无符号语义。线缆格式已冻结：wire frame v1（帧布局、字段编码、消息字段顺序均冻结，见 `crates/ak-wire` crate 文档）；整数以大端字节传输，不经 JSON/浮点表示，无精度损失。
每条消息先验证类型、版本、长度、偏移范围与资源预算，再分配内存。
单文件大小边界：默认 1 MiB 块 × 每条目 65,536 个块摘要上限 = 单文件 64 GiB；超出该边界的文件需增大块尺寸或实现单文件摘要分页/树状摘要（两者均未实现，增大块尺寸仅推迟边界——ResumeRequest 分页只拆文件条目，不拆单文件摘要向量）。
按块流式读取发送，接收端进行块摘要检查；最终按约定验证整体文件，来源于成熟摘要实现。
验证源文件在整个任务内稳定；否则报告 SOURCE_CHANGED，不拼接不同版本的数据。

## 恢复与幂等

任务 ID、文件 ID、源版本固定，恢复交换已验证持久范围。重复块与重复确认必须幂等，重连不重新生成同一任务的内容身份。
ManifestPage 每页须与 Offer 关联校验：job_id 与 Offer 一致、page_index 严格递增不重复、累计条目数/字节数不超过 Offer 宣告的 file_count/total_bytes，并受已协商清单规模绑定的内存预算约束。
Resume 恢复条目须校验：文件索引唯一、位于清单范围、已验证前缀不超过该文件实际块数；重复/越界/超界前缀显式拒绝，不得重复累加恢复统计。
接收端 ChunkAck 按待确认集合去重：重复 ACK 幂等、不重复推进确认水位。
以上三项（manifest 页关联校验、Resume 条目校验、ACK 幂等）为第十七波 F2 修复落地中（2026-11-04 审核第 3 条确认现状未完整实现：页 job_id/page_index 被忽略、Resume 索引未严格校验、ACK 不核对身份仅减计数）。
ResumeRequest 自 2026-11-03 起支持多帧分页（每帧携带条目数上限与 more 标志，接收端分片发送、发送端逐帧累积校验 job_id），单任务条目数不再受单帧上限约束。
持久确认语义（2026-11-04 与实现对齐，审核第 5 条）：ChunkAck 表示数据已写入接收端（write_all 落页缓存），不含 fsync/持久化承诺；持久边界为检查点批次（每 16 个 ACK 保存一次检查点记录）与文件 finish（整文件摘要校验＋fsync＋原子改名后才出现最终名）。崩溃恢复不直接信任检查点——`.akpart` 逐块重新哈希复核后才承认已验证前缀。
任务完成必须由接收端完成校验及最终保存后确认（完成标记严格晚于最终校验与保存提交，FinalAck 之前）。发送端断开或 UI 进度 100% 不等于完成。

## 初始错误分类

DIRECT_UNAVAILABLE、LOCAL_PERMISSION_DENIED、AUTH_FAILED、PEER_MISMATCH、ROUTE_FORBIDDEN、NETWORK_POLICY_BLOCKED、STORAGE_FULL、TARGET_REVOKED、SOURCE_CHANGED、HASH_MISMATCH、PROTOCOL_UNSUPPORTED。
用户错误提示不把推测的 NAT 类型说成精确诊断。保留可操作建议：重新授权、换网、保持两端在线、重新选择目录或重新扫描源文件。
