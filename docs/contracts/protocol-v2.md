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

`ServerError.code` 固定码表（连接级原因码，与 `PairError` 业务码分列；补登 2026-11-13 H3/CR-P3-12，语义自 `9029c08` 起已在实现中冻结）：

| code | 名称 | 语义 |
|---|---|---|
| `0x1001` | `ERR_DEVICE_REPLACED` | 本连接被同一 `device_id` 的**新**控制连接替换：新连接 `Hello` 认证通过后，服务端在关闭旧连接**之前**先向旧连接投递本原因帧（`message` 有界，如 "replaced by a newer connection"），写端排空后关闭。客户端据此把"被替换"与普通断连区分（`is_replaced`），守护进程对重连**配速**（等满替换暂停窗口再回收连接，避免两个常驻实例互踢的连接战争）；普通网络失败仍按退避第 0 步立即重连。码点选择：高于 v1 旧类错误 `1..=5`，低于 v2 冻结业务码 `0x2101..=0x2109`；不进 `PairError` 表（`pairing-flow.md` §12）。 |

`Welcome.capabilities` 是**服务端能力声明**（u32 位掩码），已定义位：`0x0001` 邀请码管理（claim/list/revoke）、`0x0002` 配对事务、`0x0004` v2 会话、`0x0008` 可查询服务器版本（G6 修订，见第 6a 节）。未定义位客户端忽略；旧客户端本就只校验 `session_ttl_secs > 0`，新位对它们是纯加法。`Hello` 无客户端能力位——capabilities 不是双方协商。

## 4. 邀请码管理消息

`InviteSummary` 固定为：

```text
code_id[16] | state:u8 | created_ms:u64 | expires_ms:u64 | revision:u64
```

共 41 字节。这里 `expires_ms=0` 表示长期有效。

| Tag | 变体 | 字段 | Payload 预算 |
|---|---|---|---:|
| `0x0A` | `InviteClaim` | `request_id:16, lookup_digest:32, ttl_secs:u32`（＋wave-28 扩展段，见第 10a 节） | ≤571 |
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
| `0x10` | `InviteRedeem` | `request_id:16, lookup_digest:32, redeemer_nonce:32, deadline_ms:u64, request_mac:32, signature:64`（＋wave-28 扩展段，见第 10a 节） | ≤699 |
| `0x11` | `InviteRedeemPending` | `request_id:16, pairing_id:16, deadline_ms:u64` | 40 |
| `0x12` | `InviteRedeemOffered` | `code_id:16, code_revision:u64, pairing_id:16, redeemer_device_id:32, redeem:InviteRedeem的固定字段`（＋wave-28 扩展段，见第 10a 节） | ≤771 |
| `0x13` | `PairChallenge` | `transcript:256, owner_proof:96` | 352 |
| `0x14` | `PairProof` | `pairing_id:16, transcript_digest:32, redeemer_proof:96` | 144 |
| `0x15` | `PairPrepared` | `pairing_id:16, transcript_digest:32, signature:64` | 112 |
| `0x16` | `PairCommit` | `transcript:256, owner_proof:96, redeemer_proof:96, owner_prepared_signature:64, redeemer_prepared_signature:64, commit_revision:u64`（＋wave-28 扩展段，见第 10a 节） | ≤1099 |
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

## 6a. G6 契约修订：ServerInfo 消息对（2026-11-13）

> **G6 契约修订轮（复核 2026-11-12 §6 F6 行）。** 纯加法：两个新 tag（`0x1F`/`0x20`，旧解码器拒绝未知 tag——但旧客户端永不发送请求、新服务端仅在请求后应答，所以任何旧×新组合都不会产生新帧）、`Welcome.capabilities` 新能力位 `0x0008`（§3）。既有帧形状零改动。

| Tag | 变体 | 字段 | Payload 预算 |
|---|---|---|---:|
| `0x1F` | `ServerInfoRequest` | 无载荷 | 0 |
| `0x20` | `ServerInfo` | `revision:u16长度+≤32B UTF-8` | ≤34 |

语义与门控：

- **仅按请求应答**：服务端永不主动推送 `ServerInfo`；客户端仅在当前连接的 `Welcome.capabilities` 置位 `0x0008` 后发送 `ServerInfoRequest`。旧服务端不置位→新客户端不发送；旧客户端忽略能力位→不发送。一切旧×新组合行为与修订前逐字节一致。
- 消息对**无 request_id**（幂等、廉价、无状态）：每连接至多一个在途查询；`revision` 是构建指纹（如 `0.1.0+4739f4d`，版本行同一来源），**不含**路径、主机名、用户数据；未配置版本的服务进程以 `unknown` 应答。
- 每连接帧预算（§11 的令牌桶）覆盖其限频，无独立限额。
- 互通降级：新客户端对只置位不应答（或回答 `ServerError`、断连）的服务端，把查询降级为"无信息"，不得影响连接其余功能。

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
| `ServerInfoRequest`（0x1F） | 客户端 → 服务端（仅 Welcome 能力位 0x0008 置位后） |
| `ServerInfo`（0x20） | 服务端 → 客户端（仅按请求应答） |

## 9. 阶段（状态）规则

- 非法状态全部拒绝（基础规则第 5 条）：消息只能在对应连接、邀请码或配对事务的当前状态下接受；状态机由服务端（A2）与客户端分派层（A3）强制，codec 层负责封闭枚举与预算验证。
- `AuthChallenge` 的 challenge 只能在当前 TLS 连接上使用一次，最长有效 10 秒（时序与新鲜度规则见 `pairing-flow.md`）。
- 已 Commit/Ready 的事务，服务端在状态回复后重发对应的固定 `PairCommit`/`PairReady`（见第 6 节状态恢复规则）。
- v2 session 按已验证的 `(pairing_id, code_id, connection_id)` 建立关联，不再只按设备对复用。

## 10. 尾随字节与其他拒绝规则

- 尾随字节全部拒绝：解码 payload 后剩余任何字节即为非法帧（第 10a 节的扩展段是声明长度**内部**的受控结构，不属于此条所述尾随字节）。
- 未定义 tag 拒绝：封闭集测试应穷举全部 256 个 tag，分别验证版本、类型、方向、长度和状态限制；不能仅把原来的“1–8 合法”改成一个更大的连续范围后结束测试。
- 超长、截断、非法阶段由生产编码/解码拒绝（A1 验证门槛）。
- 端口 0 拒绝（`SocketAddr`）；`state_filter` 仅接受已定义位、`limit` 为 1–32（第 4 节）。

## 10a. wave-28 修订：端点扩展段与单次兑换（v1 极简配对模型）

> **第二十八波修订（2026-11-06）。** 四个扩展 tag 的 payload 末尾可携带一个**可选扩展段**；其余规则不变。扩展段不是通用扩展口：只有两个已定义字段，未定义 flags 位、错误 tag 上的字段、超预算长度、段内尾随字节全部拒绝。

背景（v1 冻结产品模型）：一个随机码 = 一次传输任务，恰好两个对象。发起方（owner）在 `InviteClaim` 时上传本机连接设置；加入方（redeemer）在 `InviteRedeem` 时上传本机连接设置；服务器原样透传，使双方在**既有帧**上一次往返拿到对端设置。全程 UI 不暴露 IP/端口/指纹。

### 扩展段布局（声明长度内部，冻结字段之后）

```text
扩展段 := flags:u8 [endpoint_len:u16 + blob] [max_redeems:u32]
flags 位 0x01 = endpoint blob（四个扩展 tag 均可）
flags 位 0x02 = max_redeems:u32（仅 InviteClaim）
```

- 段不存在（冻结字段后 0 字节）＝旧形状，字节与第十八波冻结布局逐字节一致；编码侧两个字段全空时也**必须**省略整个段。
- `endpoint blob`：1..=512 字节，原样透传（建议 JSON：候选地址数组＋设备名＋方向声明），编解码与语义校验归上层；**不受** redeem/配对签名覆盖（连接提示，不是凭证）。
- `max_redeems`：0＝无限（旧默认），1＝一码一任务；编码侧仅允许挂在 `InviteClaim`。
- `flags == 0x00`、未定义位、或段内有剩余字节：拒绝。

### 挂载点与预算更新

| Tag | 帧角色 | 语义 | Payload 预算（旧 → 新） |
|---|---|---|---:|
| `0x0A InviteClaim` | owner → 服务端 | owner endpoint 存入码行；`max_redeems` 存入码行 | 52 → ≤571 |
| `0x10 InviteRedeem` | redeemer → 服务端 | redeemer endpoint 存入配对事务 | 184 → ≤699 |
| `0x12 InviteRedeemOffered` | 服务端 → owner | 携带 redeemer endpoint（随 offer 一次送达 owner） | 256 → ≤771 |
| `0x16 PairCommit` | 服务端 → 双方 | 携带 owner endpoint（redeemer 在权威 Commit 点拿到） | 584 → ≤1099 |

预算算术：`InviteClaim`＝52＋1（flags）＋2＋512＋4（u32 cap）＝571；其余三个 tag＝旧预算＋1＋2＋512。全部仍远低于 4092 帧上限。

### Rust 挂载形状（`crates/ak-signal`，类型名冻结）

```rust
pub struct EndpointBlob(pub Vec<u8>);          // ≤512B，原样透传

pub struct CtrlFrame {
    pub message: CtrlMessage,                  // 30 个变体形状不变
    pub endpoint: Option<EndpointBlob>,        // wave-28 扩展段
    pub max_redeems: Option<u32>,              // 仅 InviteClaim
}
```

扩展字段挂在**帧层**（`CtrlFrame`）而非 `CtrlMessage` 变体上：消息枚举与全部旧帧字节保持不变，`CtrlFrame::new(message)` 产生的就是旧形状；`CtrlFrame` 新增 `with_endpoint` / `with_max_redeems` 构造器。编码侧在非扩展 tag 上设置任一字段为 `EncodeError::BadExtension`。

### 单次兑换语义（与 `pairing-flow.md` §12a 同步）

- 码行新增 `max_redeems`（默认 0＝无限，兼容旧流；create 可传 1）。
- 计数口径＝该码名下 **committed/applied/ready** 状态的配对事务数；pending/aborted 不消耗额度。
- 额度在 redeem begin 与**最终 Commit 的同一写事务内**两处强制（wave-29 修订）：判定性 `PairPrepared` 写 `committed` 前重读 `max_redeems` 并重计 committed/applied/ready；多个 pending 竞争 Commit 时恰好额度内者成功，超限者同事务转 `aborted` 并由双端收到 `0x2109`（wave-30 修订 R29-P2-01：**Commit 竞争路径 scope＝Pairing，object＝pairing_id**——pairing 域错误经客户端分派扇出到对应配对收件箱/配对面，orchestrator 的等待方能真正收到终态；begin 路径（:267）保持 scope＝Code + 真实 request_id 的 waiter 路由不变）。“一码恰双方”由此在服务器提交点成立，不依赖 begin 预检或客户端限制。
- 达到上限后，新的（不同 request_id 的）redeem 立即返回 `PairError`，错误码 `0x2109 REDEEM_EXHAUSTED`（scope＝Code，object＝code_id）。
- 幂等/回放语义不变：同 `(redeemer, request_id)` 重放仍返回原 `InviteRedeemPending`（含 Commit 竞争失败后重试）；claim 侧同 `request_id` 同内容仍重放原 `InviteClaimed`。claim 幂等体的规范编码跨版本稳定：默认扩展（无 endpoint、cap 0）时与 v1 的 `lookup_digest[32]+ttl_secs[4]` 逐字节一致，仅非默认扩展追加尾部（见 `storage-schema.md` §5）。
