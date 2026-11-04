# 存储契约（服务端权威注册库 + ak-store 本地队列/journal/秘密）

> **冻结于第十八波（2026-11-04），修改需集成者核准并同步 ADR-004。**
> 事实源：`docs/plans/pairing-code-plan-part2.md` A.3、A.5.5、B.6 节。本文为 A2（服务端持久化）、A5/B1（`crates/ak-store` 队列、配对 journal、恢复映射）的契约；数字与计划原文逐字一致。

## 第一部分：服务端权威注册库（SQLite，`server-persistence` 特性）

### 1. 部署形态（冻结）

- 一个权威邀请码命名空间：所有提供邀请码登记功能的服务实例，必须使用同一个权威注册服务及其一致性存储。
- 单活注册服务；本地 SQLite 权威库，事务开启持久同步；独立服务进程持有数据库所有权锁。
- 不在共享网络文件系统上把多个 SQLite 副本拼成多活。
- 无法访问权威库时拒绝发出有效邀请码。
- 服务器持久库置于 `ak-signal` 的 `server-persistence` 特性后，默认关闭；移动客户端不因启用邀请码而被迫链接服务器 SQLite 后端。

### 2. `invite_codes` 表（冻结）

```text
invite_codes
  lookup_digest       BLOB(32) PRIMARY KEY
  code_id             BLOB(16) UNIQUE NOT NULL
  owner_device_id     BLOB(32) NOT NULL
  created_unix_ms     INTEGER NOT NULL
  expires_unix_ms     INTEGER NULL
  state               active | revoked | expired
  revision            INTEGER NOT NULL
  create_request_id   BLOB(16) NOT NULL

  UNIQUE(owner_device_id, create_request_id)
```

约束语义（来自 A.3）：

- 服务端不接收、不保存明文邀请码：表中只有 `lookup_digest`，没有也不得增加明文码、`auth_key` 或 MAC 材料。
- claim 的线性化点是唯一索引保护下的 INSERT 事务提交；同码并发 claim 最多一个成功，其余 `CODE_CONFLICT`。
- 幂等：同 owner、同 `request_id`、同请求内容返回原结果；同 owner、同 `request_id`、不同内容拒绝 `REQUEST_CONFLICT`；同 lookup、不同请求不能覆盖原 owner。
- 撤销、过期后不复用旧码：保留最小 tombstone（至少 lookup、终态及必要版本信息），不允许通过删除记录重新激活旧码。
- `expires_unix_ms` 为 NULL（或协议层 `expires_ms=0`）表示长期有效。

### 3. `pairing_transactions` 表（冻结）

计划原文定义其内容为：

> `pairing_transactions`：固定配对 transcript、双方证明、Prepared/Commit/Applied 状态。

据此冻结：

- 必须能存储固定 256 字节 `PairTranscript`（布局见 `protocol-v2.md` 第 5 节）。
- 必须能存储双方证明（`PairProofData` 各 96 字节：owner_proof、redeemer_proof）。
- 事务状态为 **Prepared / Commit / Applied** 生命周期（消息驱动时序见 `pairing-flow.md` 第 6 节）；已持久 Commit 的事务必须恢复到完成或明确人工处理，不能按普通 TTL 丢弃。
- 撤销与配对 Commit 在同一权威事务域内排序：Revoke 先提交则尚未 Commit 的配对失败；Commit 先提交则此次配对保留。
- 计划未逐列列举该表全部列；具体 DDL 列由 A2 实现时按上述冻结内容落地，新增列不得超出“配对事务状态与证明”职责（不保存文件任务记录、路径、内容或文件进度）。

### 4. `pairing_relations` 表（冻结）

计划原文定义其内容为：

> `pairing_relations`：已完成配对的 owner/redeemer、公钥、`code_id` 与 `pairing_id` 关联。

据此冻结：

- 记录已完成配对的双侧设备身份（owner/redeemer）、双方公钥、关联的 `code_id` 与 `pairing_id`。
- 同一码允许 owner 与多个 redeemer 分别配对（A↔B、A↔C 各成一条关系），不产生 redeemer 间关系。
- 名下配对关系受有界存储配额约束（建议首版 1024，达到上限明确拒绝）。
- 计划未逐列列举该表全部列；具体 DDL 列由 A2 实现时按上述冻结内容落地。

### 5. 幂等记录与 schema 版本（冻结）

- 有界请求幂等记录：支撑“同 owner、同 `request_id`、同请求内容返回原结果；不同内容拒绝 `REQUEST_CONFLICT`”，记录有界并回收。
- 注册库自带 schema 版本；升级迁移不得破坏 tombstone 与唯一性记录。
- 不保存文件任务记录、路径、内容或文件进度。

### 6. 备份与时钟规则（冻结）

- 过期判断在每次 redeem 与 Commit 事务内执行，不能只依赖定时清扫。
- 已经进入 expired/revoked 的状态不可因时钟回拨恢复 active。
- 服务器检测到异常时钟回退或数据库恢复缺口时，应停止发行和新的配对提交，先完成状态核对。
- 从旧备份恢复后，不能立即继续发码并声称唯一性完整；必须恢复全部已确认登记及撤销记录，或保持发行关闭。

## 第二部分：ak-store 本地队列、journal 与秘密（B.6/A.5.5）

### 7. 持久化排队任务记录（冻结）

默认持久化排队任务。本地记录包含：

```text
schema_version
transfer_id
request_id
registry_id / code_id / pairing_id
peer_fingerprint
direction
source_refs / target_ref
transfer_options
state
enqueue_sequence
policy_revision
checkpoint_record_id
last_stable_error_code
```

源路径、目标路径是**本地私有数据**，仅写用户身份目录，不写协调数据库、日志或仓库。

恢复规则：

- 尚未运行的本地队列项恢复为 Queued。
- 上次运行中的任务恢复为 WaitingDirect/待恢复状态。
- 重新检查源文件、目录权限、配对关系和目标权限。
- 不恢复旧 QUIC 连接、候选可信结论或旧许可。
- Android 授权失效进入 WaitingPermission。
- 已取消任务不自动复活。
- 持久 Completed 仍必须晚于最终校验与提交。
- 提供 `--no-persist-queue` 临时模式，并明确进程退出后排队项丢失。

服务器不保存传输队列。接收端保存已经启动任务的检查点；远端尚未获准的请求可保持有界临时状态，由发送端恢复后重新提交。

### 8. 配对 journal（冻结）

本地 journal 记录 **Prepared、CommitSeen、BookApplied、Ready** 四个持久边界：

- 与两侧落簿规则（`pairing-flow.md` 第 9 节）配套：新增记录在 `PairReady` 前不能通过邀请码路径放行任务；准入同时检查配对 journal，不能只看 `book.get(fp).is_some()`。
- 延迟 Commit 不能复活用户已经移除的信任；用本地信任版本或 tombstone 拒绝旧事务重放。
- 一侧写失败时显示“配对尚未完成”；已 Commit 的事务重连后继续恢复，不能超时假报成功或盲目删除另一侧已保存的数据。
- A5/B1 验证门槛：每个落盘边界崩溃恢复；坏记录失败关闭；不污染文件检查点。

### 9. record_id 隔离与 `AKREC2`（冻结）

第十七波已有：

```text
wire_job_id = 内容身份

record_id = BLAKE3(
  content_id || peer || direction || root
)[..16]
```

它解决不同对端、方向、根目录之间的覆盖，但两个“同源、同对端、同目标”的独立并行任务仍可能得到同一记录键。

新任务增加随机、持久的 `transfer_id`：

- 用户每次提交新任务生成新实例。
- 同实例重试沿用。
- 用户“再发送一份”生成新实例。
- 新记录采用 `AKREC2` 域分离，包含 `transfer_id` 和现有全部身份维度。
- 原 `AKREC1` 恢复兼容保留，不能把一个旧记录同时分配给两个实例。
- FFI 与 CLI 明确区分 `task_id/transfer_id`、`wire_job_id`、`record_id`、`pairing_id`、`code_id`。

### 10. `.akpart` 隔离（并发前置，冻结）

当前 `.akpart` 主要按目标路径派生。即使 checkpoint 不同，两个任务仍可能打开同一个 part 文件。

安全并行的前置任务包括：

```rust
prepare_file_for_transfer(
    entry,
    conflict,
    staging_namespace,
)
```

要求：

- staging 绑定 `transfer_id`，同实例恢复沿用。
- 最终目标名存在独立竞争控制。
- `Fail` 与 `Rename` 都不能共享写入中的 part。
- 取消不能删除其他任务的 part。
- 路径边界和符号链接竞态防护必须落在实际打开/提交操作，不能只替换字符串。
- 继续保留 hard-link no-replace 提交。
- 对不支持可靠原子提交的卷，明确拒绝该能力或实现并验证平台替代；不能默默退回存在覆盖竞态的 rename，然后宣称并行安全。

### 11. 本地邀请码秘密（冻结）

`peers.book` 继续只保存公开信任材料。邀请码秘密独立存储，元数据只记录 `secret_ref`。

首版可以沿用现有设备种子文件的保护级别：用户私有身份目录、严格访问权限、排他更新、原子写入，秘密不进入普通导出及日志。必须如实注明：这不能防止已经取得同一用户文件读取权限的攻击者。

存储接口预留系统秘密存储后端；Windows DPAPI、Android Keystore、Linux Secret Service 的接入与验收单独记录，不能把接口存在写成系统保护已经实现。

### 12. 验证门槛（A5/B1 对照）

- A5：pending/committed/applied/ready、秘密引用、幂等恢复；每个落盘边界崩溃恢复；坏记录失败关闭；不污染文件检查点。
- B1：队列 schema、journal、AKREC2 恢复映射；原子更新、崩溃恢复、同实例重试、不同实例隔离、旧记录兼容。
- A2（服务器侧）：并发碰撞只成功一次；重启不重码；撤销/Commit 竞态；库满、损坏、慢客户端；TLS 错 pin。
