# 控制协议 v2 契约（codec 冻结）

> **冻结于第十八波（2026-11-04），修改需集成者核准并同步 ADR-004。**
> 事实源：`docs/plans/pairing-code-plan-part2.md` A.4 节。本文为 A1（`crates/ak-signal/src/protocol_v2/`）的唯一 codec 契约；数字与计划原文逐字一致。

## 1. 基础规则（冻结）

采用独立 `codec::v2::CtrlMessage`，保留 v1 解码器及其原始布局。新邀请码功能仅通过 **TLS 控制协议 v2** 使用，不自动降级明文。

- 帧头仍为 4 字节，帧版本为 `2`。
- 全帧仍不超过 **4096 字节**。
- 下表预算都是 **payload 字节数，不含 4 字节帧头**。
- 整数为大端编码；固定数组不带长度前缀。
- 未定义 tag、非法方向、非法状态、尾随字节全部拒绝。
- 先验证类型和长度预算，再分配。
- 新编码入口使用可返回错误的 `try_encode`，不能靠截断长度或仅有 `debug_assert` 保证正确性。

不能将扩展字段直接追加到现有 v1 帧后，让旧解码器猜测。

## 2. 类型别名（冻结）

```rust
type RequestId    = [u8; 16];
type CodeId       = [u8; 16];
type PairingId    = [u8; 16];
type ConnectionId = [u8; 16];
type SessionId    = [u8; 16];

type DeviceId     = [u8; 32]; // Ed25519 公钥
type RegistryId   = [u8; 32]; // 权威服务身份
type Digest32     = [u8; 32];
type Nonce32      = [u8; 32];
type Mac32        = [u8; 32];
type Signature64  = [u8; 64];
```

所有随机 ID 与 nonce 使用 CSPRNG；数据库中的 ID 另有唯一约束。全零 ID 保留为分页起止等明确哨兵，不发行给真实对象。

## 3. 既有流程在 v2 中的布局

| Tag | `CtrlMessage` 变体 | 字段，按编码顺序 | Payload 预算 |
|---|---|---|---:|
| `0x01` | `Hello` | `version:u16, device_id:32, name:u16长度+≤64B, timestamp_ms:u64, challenge:32, signature:64` | ≤204 |
| `0x02` | `Welcome` | `session_ttl_secs:u32, capabilities:u32` | 8 |
| `0x03` | `Register` | `count:u8, candidates:[SocketAddr; ≤16]` | ≤305 |
| `0x04` | `ConnectRequest` | `request_id:16, target:32, pairing_id:16, code_id:16, connection_id:16` | 96 |
| `0x05` | `SessionOpened` | `session:16, peer:32, pairing_id:16, code_id:16, connection_id:16, expires_ms:u64` | 104 |
| `0x06` | `Signal` | `session:16, signal:PeerSignal` | ≤322 |
| `0x07` | `Cancel` | `session:16` | 16 |
| `0x08` | `ServerError` | `code:u16, message:u16长度+≤128B` | ≤132 |
| `0x09` | `AuthChallenge` | `registry_id:32, nonce:32, expires_ms:u64` | 72 |

这些是 **v2 布局**。不能将扩展字段直接追加到现有 v1 帧后，让旧解码器猜测。

`SocketAddr` 延续现有编码：地址族 1 字节、IPv4/IPv6 地址 4/16 字节、端口 2 字节，因此每项最大 19 字节。端口 0 拒绝。

`ServerError` 保留有界旧类错误；邀请码业务错误走下面的固定 `PairError`，不回显用户输入。

## 4. 邀请码管理消息

`InviteSummary` 固定为：

```text
code_id[16] | state:u8 | created_ms:u64 | expires_ms:u64 | revision:u64
```

共 41 字节。这里 `expires_ms=0` 表示长期有效。

| Tag | 变体 | 字段 | Payload 预算 |
|---|---|---|---:|
| `0x0A` | `InviteClaim` | `request_id:16, lookup_digest:32, ttl_secs:u32` | 52 |
| `0x0B` | `InviteClaimed` | `request_id:16, code_id:16, created_ms:u64, expires_ms:u64, revision:u64` | 56 |
| `0x0C` | `InviteList` | `request_id:16, cursor:16, limit:u8, state_filter:u8` | 34 |
| `0x0D` | `InviteListPage` | `request_id:16, next_cursor:16, count:u8, entries:[InviteSummary; ≤32]` | ≤1345 |
| `0x0E` | `InviteRevoke` | `request_id:16, code_id:16, expected_revision:u64` | 40 |
| `0x0F` | `InviteRevoked` | `request_id:16, code_id:16, revision:u64, revoked_ms:u64` | 48 |

约束：

- owner 从已认证控制连接推导，不能由请求任意指定。
- list 只列本设备创建的码。
- list 不返回明文码、lookup、MAC 或服务器上的私有路径。
- 本地备注不进入协调协议。
- `state_filter` 仅接受已定义位；`limit` 为 1–32。
- 只有 owner 可以 revoke。访问他人的 `code_id` 与不存在的 ID 返回相同不可用错误。

## 5. 配对 transcript 与证明

冻结 `PairTranscript`，共 **256 字节**：

```text
registry_id[32]
code_id[16]
code_revision:u64
pairing_id[16]
request_id[16]
lookup_digest[32]
owner_device_id[32]
redeemer_device_id[32]
owner_nonce[32]
redeemer_nonce[32]
deadline_ms:u64
```

冻结：

```text
PairProofData = mac[32] + signature[64]     // 96 字节
```

MAC 和签名覆盖明确的角色域及完整规范 transcript：

```text
owner_mac = MAC(auth_key, "AK owner proof v1" || transcript)
redeemer_mac = MAC(auth_key, "AK redeemer proof v1" || transcript)

signature = Ed25519.Sign(
    device_key,
    对应角色域 || transcript || mac
)
```

使用库提供的成熟 MAC/派生实现和恒定时间验证接口，不手写密码学原语。整个认证协议仍需独立审阅与固定测试向量，不能仅凭采用成熟哈希算法就宣称协议已经被证明安全。

## 6. Redeem 与双侧落簿消息

| Tag | 变体 | 字段 | Payload 预算 |
|---|---|---|---:|
| `0x10` | `InviteRedeem` | `request_id:16, lookup_digest:32, redeemer_nonce:32, deadline_ms:u64, request_mac:32, signature:64` | 184 |
| `0x11` | `InviteRedeemPending` | `request_id:16, pairing_id:16, deadline_ms:u64` | 40 |
| `0x12` | `InviteRedeemOffered` | `code_id:16, code_revision:u64, pairing_id:16, redeemer_device_id:32, redeem:InviteRedeem的固定字段` | 256 |
| `0x13` | `PairChallenge` | `transcript:256, owner_proof:96` | 352 |
| `0x14` | `PairProof` | `pairing_id:16, transcript_digest:32, redeemer_proof:96` | 144 |
| `0x15` | `PairPrepared` | `pairing_id:16, transcript_digest:32, signature:64` | 112 |
| `0x16` | `PairCommit` | `transcript:256, owner_proof:96, redeemer_proof:96, owner_prepared_signature:64, redeemer_prepared_signature:64, commit_revision:u64` | 584 |
| `0x17` | `PairApplied` | `pairing_id:16, transcript_digest:32, commit_revision:u64, signature:64` | 120 |
| `0x18` | `PairReady` | `pairing_id:16, transcript_digest:32, commit_revision:u64, owner_applied_signature:64, redeemer_applied_signature:64` | 184 |
| `0x19` | `PairStatusQuery` | `request_id:16, pairing_id:16` | 32 |
| `0x1A` | `PairStatusReply` | `request_id:16, pairing_id:16, state:u8, commit_revision:u64, deadline_ms:u64` | 49 |
| `0x1B` | `PairAbort` | `request_id:16, pairing_id:16, reason:u16` | 34 |
| `0x1C` | `PairError` | `request_id:16, scope:u8, object_id:16, code:u16, retry_after_ms:u32` | 39 |
| `0x1D` | `Ping` | `nonce:16` | 16 |
| `0x1E` | `Pong` | `nonce:16` | 16 |

`scope`、`state`、`reason` 都是封闭枚举。`InviteRedeemOffered.redeem` 是固定结构，不能实现为字节数组或通用嵌套消息。

状态恢复规则：

- `PairStatusQuery` 仅允许配对参与方查询。
- 已 Commit/Ready 的事务，服务端在状态回复后重发对应的固定 `PairCommit`/`PairReady`。
- 不增加 `extra_json`、`metadata`、`attachment` 等扩展口。
- 对合法但不存在或无权查询的对象，返回统一不可用结果。

## 7. `PeerSignal` 子类型

本功能**不向 `PeerSignal` 增加邀请码或通用认证载荷**。配对消息由上面的明确 `CtrlMessage` 变体承载。

| 子 tag | 变体 | 编码内容 | 含子 tag 的最大字节数 |
|---|---|---|---:|
| `0x01` | `Candidates(Vec<SocketAddr>)` | tag、数量、至多 16 个地址 | 306 |
| `0x02` | `PunchAt { epoch_unix_ms, nonce }` | tag、u64 时间、8B nonce | 17 |
| `0x03` | `Bye` | tag | 1 |
| `0x04` | `SameEgress` | tag | 1 |

`SameEgress` 仍只能由服务端生成，仍只是排序提示。它不证明两端位于可信局域网。

## 8. 方向规则

每条消息只允许下表列出的方向；非法方向拒绝（基础规则第 5 条）。方向取自计划 A.5.3 时序图中的角色：A＝码持有方 owner，B＝输入方 redeemer，S＝协调服务。

| 消息 | 允许方向 |
|---|---|
| `Hello`（0x01） | 客户端 → 服务端 |
| `Welcome`（0x02） | 服务端 → 客户端 |
| `Register`（0x03） | 客户端 → 服务端 |
| `ConnectRequest`（0x04） | 客户端 → 服务端 |
| `SessionOpened`（0x05） | 服务端 → 客户端 |
| `Signal`（0x06） | 双向（客户端 ⇄ 服务端） |
| `Cancel`（0x07） | 客户端 → 服务端 |
| `ServerError`（0x08） | 服务端 → 客户端 |
| `AuthChallenge`（0x09） | 服务端 → 客户端 |
| `InviteClaim`（0x0A） | owner → 服务端 |
| `InviteClaimed`（0x0B） | 服务端 → owner |
| `InviteList`（0x0C） | owner → 服务端 |
| `InviteListPage`（0x0D） | 服务端 → owner |
| `InviteRevoke`（0x0E） | owner → 服务端 |
| `InviteRevoked`（0x0F） | 服务端 → owner |
| `InviteRedeem`（0x10） | redeemer → 服务端 |
| `InviteRedeemPending`（0x11） | 服务端 → redeemer |
| `InviteRedeemOffered`（0x12） | 服务端 → owner |
| `PairChallenge`（0x13） | owner → 服务端；服务端 → redeemer |
| `PairProof`（0x14） | redeemer → 服务端；服务端 → owner |
| `PairPrepared`（0x15） | 配对参与方 → 服务端 |
| `PairCommit`（0x16） | 服务端 → 配对参与方（含状态恢复重发） |
| `PairApplied`（0x17） | 配对参与方 → 服务端 |
| `PairReady`（0x18） | 服务端 → 配对参与方 |
| `PairStatusQuery`（0x19） | 配对参与方 → 服务端 |
| `PairStatusReply`（0x1A） | 服务端 → 配对参与方 |
| `PairAbort`（0x1B） | 配对参与方 → 服务端 |
| `PairError`（0x1C） | 服务端 → 配对参与方 |
| `Ping`（0x1D） | 客户端 → 服务端 |
| `Pong`（0x1E） | 服务端 → 客户端 |

## 9. 阶段（状态）规则

- 非法状态全部拒绝（基础规则第 5 条）：消息只能在对应连接、邀请码或配对事务的当前状态下接受；状态机由服务端（A2）与客户端分派层（A3）强制，codec 层负责封闭枚举与预算验证。
- `AuthChallenge` 的 challenge 只能在当前 TLS 连接上使用一次，最长有效 10 秒（时序与新鲜度规则见 `pairing-flow.md`）。
- 已 Commit/Ready 的事务，服务端在状态回复后重发对应的固定 `PairCommit`/`PairReady`（见第 6 节状态恢复规则）。
- v2 session 按已验证的 `(pairing_id, code_id, connection_id)` 建立关联，不再只按设备对复用。

## 10. 尾随字节与其他拒绝规则

- 尾随字节全部拒绝：解码 payload 后剩余任何字节即为非法帧。
- 未定义 tag 拒绝：封闭集测试应穷举全部 256 个 tag，分别验证版本、类型、方向、长度和状态限制；不能仅把原来的“1–8 合法”改成一个更大的连续范围后结束测试。
- 超长、截断、非法阶段由生产编码/解码拒绝（A1 验证门槛）。
- 端口 0 拒绝（`SocketAddr`）；`state_filter` 仅接受已定义位、`limit` 为 1–32（第 4 节）。
