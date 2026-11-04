# ADR-004：邀请码配对、权威唯一性与共享任务调度

- 状态：已接受（2026-11-04，第十八波契约冻结波。本 ADR 与 `docs/contracts/` 下五份契约同批冻结；实现自第十九波起按契约并行推进，落地证据由各波执行者按 `docs/plans/pairing-code-plan-part2.md` A.7/B.9 的验证门槛补齐，并由集成者收口记账）
- 关联：ADR-001（仅设备直连承载文件数据，**不变**）；ADR-002（QUIC＋自建信令路线、受控依赖清单、契约先行并行开发模式）；ADR-003（句柄式 C ABI 边界）
- 事实源：`docs/plans/pairing-code-plan-part2.md`（唯一事实源，本 ADR 及全部契约的数字、字段、枚举均逐字取自该计划）

## 背景

第十七波后，产品需要跨网络的首配对入口：当前 `pairing_code(fingerprint)` 生成的 `123-456` 六位短码只用于已配对设备间的指纹核对，不能承载“第一次把两台陌生设备连起来”的授权。同时，`ak-cli` daemon 与 Flutter GUI 各自为政地驱动文件接收（`receive`、`receive_with_addr`、`serve_concurrently`），缺少统一的常驻任务调度、并发策略与背压层。

2026-11-04 Codex 计划（`docs/plans/pairing-code-plan-part2.md`）给出 A（协调面协议设计）与 B（Rust 调度层）两部分方案。按 AGENTS.md“并行开发规则”第 3 条（公共 API 契约先行冻结再并行），第十八波将其中所有跨模块接口冻结为本 ADR 与 `docs/contracts/` 契约文档；A1–A7、B0–B11 各执行者只能依据冻结契约实现，接口变更必须先改契约文档并通告集成者。

## 决策

### 1. 配对邀请码：客户端生成、服务端登记

新功能统一命名为“配对邀请码”，代码类型 `InviteCode`，字段 `invite_code`。既有 `123-456` 六位码保持兼容，界面与文档改称“指纹核对短码”；新邀请码不进入旧六位码解析分支，也不成为 `receive.expect` 的隐式准入值。

- **生成方**：客户端生成、服务端登记。客户端用操作系统 CSPRNG（Rust `OsRng`）生成 20 字节（160 bit 熵）随机，Crockford Base32 编码为 `AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX`（`AKI1` 为格式版本，不计入随机熵）。不使用 `ak-signal::util::unique_bytes`（时钟与计数器派生工具，非密码学随机源）。
- **否决服务端生成**：服务器接触全部配对凭证，数据库、日志或服务端被侵入时风险扩大。
- **随机性防猜测，唯一性由权威数据库事务保证**：不把“碰撞概率很低”写成“绝不重复”；claim 线性化点是唯一索引保护下的 INSERT 事务提交，碰撞重试单次操作最多 8 次。

### 2. lookup/auth 双派生，服务端不见明文码

```text
lookup_digest = BLAKE3_DERIVE_KEY("AK invite lookup v1", secret20)
auth_key      = BLAKE3_DERIVE_KEY("AK invite authentication v1", secret20)
```

服务器只接收 `lookup_digest`；`auth_key` 留在两个端点，用于配对 transcript 的 MAC。lookup 不能直接当配对凭证。MAC 与签名构造、transcript 布局在 `docs/contracts/protocol-v2.md` 与 `docs/contracts/pairing-flow.md` 冻结；使用库提供的成熟 MAC/派生实现和恒定时间验证接口，不手写密码学原语，整个认证协议仍需独立审阅与固定测试向量。

### 3. 权威单活 SQLite 注册

- 一个权威邀请码命名空间：所有提供登记功能的服务实例使用同一个权威注册服务及其一致性存储。
- 首版：单活注册服务、本地 SQLite 权威库（事务开启持久同步）、独立服务进程持有数据库所有权锁、不在共享网络文件系统上拼多活、无法访问权威库时拒绝发码。
- 首版不支持“每个私服独立发行相同格式、同时声称全局唯一”；未来支持私服时统一接入权威注册，或在完整邀请码中加入权威唯一分配的发行方命名空间。
- 撤销、过期后不复用旧码：保留最小 tombstone（lookup、终态及必要版本信息），不允许删除记录重新激活。
- 服务器持久库置于 `ak-signal` 的 `server-persistence` 特性后，默认关闭；移动客户端不因启用邀请码被迫链接服务器 SQLite 后端。
- 表结构（`invite_codes`/`pairing_transactions`/`pairing_relations`/幂等记录/schema 版本）冻结于 `docs/contracts/storage-schema.md`。

### 4. 控制协议 v2 仅经 TLS，不自动降级明文

- 新邀请码功能仅通过 TLS 控制协议 v2 使用；保留 v1 解码器及其原始布局，不将扩展字段追加到 v1 帧。
- 帧头仍 4 字节、帧版本 `2`、全帧上限 4096 字节、先验证类型与长度预算再分配、未定义 tag/非法方向/非法状态/尾随字节全部拒绝。
- TLS 验证协调服务身份并绑定固定 `RegistryId`；首版可采用明确配置的服务端证书公钥 pin，不得自动接受未知 pin，也不得降级现有明文 TCP。
- Hello v2 签名绑定 `"AKHELLO2" || registry_id || TLS exporter || Hello 规范字段（不含 signature）`；`AuthChallenge` 只在当前 TLS 连接上使用一次，最长有效 10 秒。
- 仅有 TLS 不足以防止被入侵的协调服务替换首次配对公钥，因此叠加邀请码派生的双端证明（owner/redeemer 各自的 MAC＋Ed25519 签名，覆盖角色域与完整规范 transcript）。
- 消息表、字段、预算、方向与状态规则冻结于 `docs/contracts/protocol-v2.md`；redeem 时序、错误码、防暴力参数冻结于 `docs/contracts/pairing-flow.md`。

### 5. 共享调度层 `ak-session`：两维并发

新增共享 `ak-session` crate（含 `scheduler`、`pairing`、`runtime` 模块），CLI 与 FFI 共用；不把产品调度策略分别放进 CLI daemon 和 Flutter，不把邀请码生命周期塞进文件协议引擎，暂不拆第二个 `ak-scheduler` crate。

- 分组键 `CodeGroupKey = (registry_id, code_id)`；一码对应一个 owner，owner 对该码产生的全部关系（A→B、B→A、A→C、C→A）统一执行同码策略。
- 两维并发：`same_code_mode`（`serial`|`parallel`，**默认 `serial`**）× `cross_code_mode`（`parallel`|`serial`，**默认 `parallel`**）。serial 模式覆盖对应并行上限；两端策略取交集（一端并行、一端串行，实际串行）。
- `serve_concurrently(spec, expected_jobs, progress)` 外部语义保留为兼容批处理入口；`expected_jobs` 是本轮接受并结束的连接数，不是并发上限，不得解释成新调度器并发上限。
- 首版保持一任务一个 QUIC connection；任务内 multistream 是独立设置，不表达跨任务并发。
- 双端任务准入（`ak-wire` 新 capability `CAP_TASK_ADMISSION`，`TaskOpen`/`TaskAdmission`/`TaskAdmissionAck` 封闭消息，仅经设备直连、不经 `ak-signal`）；新邀请码任务遇不支持 capability 的旧端返回 `PROTOCOL_UNSUPPORTED`，不退回无分组准入路径。
- 并发矩阵、配置 JSON 及范围、TaskState 状态机、permit 准入模型冻结于 `docs/contracts/scheduler.md`。

### 6. 新增受控依赖：决策框架，不写死 TLS 库

按 ADR-002 第 5 条的受控清单原则执行，本 ADR 只冻结选择框架：

- **准入标准**（任何候选必须全部满足）：纯 Rust 或 MSVC 可编译；不引入 relay/TURN/WebRTC 类依赖（对照 ADR-001，经 `scripts/check_repository.py` 的 Cargo.lock 扫描）；支持 TLS 控制连接所需能力（服务端证书公钥 pin 验证、TLS exporter 派生、按 A.5 要求的握手与关闭行为）；MAC/派生使用成熟库实现与恒定时间验证接口。
- **定夺流程**：TLS 库具体选型（含 SQLite 绑定等持久化依赖）由 B3（`ak-net`）及相关执行者在实现时按计划 A.5 的建议评估并列出候选与理由，报集成者核准后更新锁文件；受控新增依赖由集成者更新锁文件，之后验证使用 `--locked`。
- **复核义务**：新依赖落地时重新检查 TLS 配置、代理使用、relay 默认值、权限和协议兼容（AGENTS.md“依赖升级”条 + A.6 补充边界）；依赖矩阵不把服务器库泄漏到移动端。
- 参照系：数据面 QUIC 已按 ADR-002 使用 quinn＋rustls（ring 后端）；控制面 TLS 是否复用同一栈，属 B3 候选评估范围。

### 7. 与 ADR-001/002/003 的关系

- **ADR-001 不变**：协调面仍只处理公开设备身份、邀请码查找摘要、固定长度认证证明、配对事务状态、连接候选与打洞参数；文件正文、密文、路径、文件名、清单、内容摘要、文件任务进度均不进入协调面（`transcript_digest` 是配对握手记录摘要，不是文件摘要）。仍禁止 TURN/relay、HTTP 文件上传、云端暂存、第三设备代转、隐藏代理回退；协调 session 结束、邀请码到期或服务器退出，不主动中断已稳定且仍获本地授权的数据直连。
- **补充 ADR-002**：TLS 控制面 v2 与服务器持久库作为受控新增依赖（决策框架见上节）；`ak-session` 加入模块拆分清单，依赖方向 `ak-cli`/`ak-ffi` → `ak-session` → `ak-signal`/`ak-identity`/`ak-store`，`ak-session` → `ak-engine` → `ak-net`/`ak-wire`/`ak-storage`。
- **增补 ADR-003**：新增 `ak_pairing_*` 与 `ak_sched_*` 两族 C ABI 导出、JSON schema、事件与错误码语义，冻结于 `docs/contracts/ffi-v2.md`；句柄式、调用方缓冲区、原生异步执行、轮询取结果的边界形态不变，`unsafe` 仍压缩在 `ak-ffi` shim 一层。ADR-003 原文不因本波改动，其“新 ABI 提升版本并公布能力”的落地由 B8 执行者按本 ADR 引用的契约执行、集成者回填证据。

## 备选方案与取舍

- **服务端生成邀请码**：分配流程简单，但服务器接触全部配对凭证，被侵入时风险扩大——不采用。
- **每私服独立发行同格式码**：互不通信的独立服务器无法靠随机数保证世界范围不重复——首版不支持。
- **把调度策略放 CLI/Flutter 各自实现**：两端策略漂移、无法取交集——不采用；改为共享 `ak-session`。
- **单独拆 `ak-scheduler` crate**：配对关系、连接生命周期与队列高度相关，首版在一个共享应用层内分模块更易保持一致——暂不拆。
- **把 `receive --jobs N` 解释为新调度并发上限**：破坏既有批处理语义——禁止。
- **对不支持 `CAP_TASK_ADMISSION` 的旧端退回无分组准入**：绕过同码串行策略——禁止，返回 `PROTOCOL_UNSUPPORTED`。

## 后果

- 各模块（A1–A7、B0–B11）可按冻结契约在第十九波起全并行开发，公共接口变更必须先改契约文档并通告集成者。
- 邀请码是 bearer credential：码泄露后持有者即具配对能力，不承诺区分“原本想邀请的人”与“窃取了码的人”；需要核验具体人的场景仍独立核对完整设备指纹。
- owner 必须在线自动处理兑换（码本身即授权，不需二次手动同意）；owner 离线时本次尝试进入等待并按期限失败。
- TLS pin、SQLite 权威库、系统秘密存储等运维要求成为部署前提；缺少 TLS 或注册库时不能静默退回明文邀请码服务。
- 服务器错误响应统一不泄露码存在性（`PAIR_UNAVAILABLE` 统一终态），不声称完全消除所有时序侧信道。

## 回退原则

回退以**关闭新邀请码入口、停止新任务准入、保留数据库和本地恢复记录**为原则：

- 不能删除唯一性 tombstone；
- 不能恢复隐式 `expect:any`；
- 不能降级明文传邀请码；
- 不能恢复 relay（ADR-001 永久约束）。

已有稳定直连按原授权继续；新建直连失败则等待、暂停或重试。

## 验证

本 ADR 的契约由下列文档承载，修改任一契约需集成者核准并同步本 ADR：

- `docs/contracts/protocol-v2.md`（A1 契约：v2 消息表、字段、预算、方向/状态/尾随规则）
- `docs/contracts/pairing-flow.md`（A2/A4/A6 契约：claim/redeem/revoke 时序、状态机、错误码、防暴力参数）
- `docs/contracts/scheduler.md`（B0/B6 契约：两维并发矩阵、TaskState、permit 准入）
- `docs/contracts/ffi-v2.md`（B8 与 Dart 绑定契约：C ABI、JSON schema、事件、错误码）
- `docs/contracts/storage-schema.md`（A2/A5/B1 契约：服务器注册表与本地队列/journal schema）

各实现波的验证门槛（含 `python scripts/check_repository.py`、Rust 测试、静态检查与平台实际构建）按计划 A.7/B.9/B.10 执行并记录实际命令、退出结果与缺失环境。
