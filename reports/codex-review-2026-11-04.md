# AK-sending 完整项目审核报告（Codex / gpt-6-astra / reasoning=max，2026-11-04）

> 只读审核；静态代码结论，未运行构建/测试。审核范围：全部 12 个 Rust crate + 依赖链 + 文档证据链。
> 本文件为审核原文归档；修复立项见同日提交与后续验证记录。

本轮只读审阅了 `ak-engine`、`ak-wire`，并沿调用链核对 `ak-storage`、`ak-store`、`ak-core` 与相关 `ak-itests`。没有修改文件，没有运行构建、测试或动态攻击；以下均为静态代码结论。

1. **P1：接收端未约束 `Offer.chunk_size`，小型合法消息能触发接近 4 GiB 的分配。**

   - `crates/ak-wire/src/lib.rs:811` 直接读取任意 `u32`。
   - `crates/ak-engine/src/lib.rs:1701` 只检查非零，`:1765` 将其传入存储层。
   - `crates/ak-storage/src/lib.rs:613` 执行 `vec![0u8; chunk_size.get() as usize]`，`:735` 最终校验再次按同样大小分配。
   - 触发条件：已获接收授权的对端发送 `chunk_size = u32::MAX`，然后一项 `size = 0, chunk_digests = []` 的文件条目。空文件满足存储层块数一致性检查，却仍申请约 4 GiB。帧本身很小，绕过 `Limits.max_chunk_bytes = 8 MiB` 的数据帧限制。实际内存耗尽结果取决于操作系统提交策略；未运行 OOM 实验。
   - 修复建议：Offer 到达后、Accept 发出前检查 `1..=limits.max_chunk_bytes`；存储 API 同样防御非法块尺寸；空文件不申请块缓冲区，尾块缓冲按实际需要分配。加跨层小帧负例，不实际申请巨额内存。

2. **P1/P2：正常多流传输持续超过 `idle_timeout` 后，控制流被误判为空闲并关闭，最终无法完成。**

   - `crates/ak-engine/src/lib.rs:2302` 对控制流单独执行 `timeout(idle, rx.recv_frame(...))`；`:2304` 超时直接退出任务、丢弃控制流接收半边。
   - 正常发送端在所有文件流传完后才发送 Finalize：`:1494`–`:1500`。中间控制流本来就可能超过默认 30 秒没有消息。
   - 多流主循环的 `last_activity` 虽由数据事件刷新（`:2346`、`:2367`），却不影响上述独立控制流计时器。因此持续有文件数据进展也会失去接收 Finalize 的通道。
   - 修复建议：统一连接/任务级空闲监控，任何合法数据流活动刷新共同预算；控制流读取任务持续存活到任务取消或终结，并受统一取消令牌管理。
   - 验证建议：缩短 idle，每个数据块间隔均小于 idle，总传输时间大于 idle，再 Finalize，应成功。现有短小多流环回测试覆盖不到这一条件。

3. **P2：跨帧资源预算及协议关联验证不完整。**

   - Manifest 累积：`crates/ak-engine/src/lib.rs:1671`–`:1708` 忽略每页 `job_id`、`page_index`，不校验 Offer 的 `file_count/total_bytes`，无累计条目/元数据内存预算。
   - Resume 累积：`:636`–`:647` 只验证 `job_id` 后 `self.entries.extend(files)`；无限 `more=true` 可一直增长。处理结果时 `:1386`–`:1390` 不拒绝重复索引、越界索引、超过文件实际块数的 prefix；重复值会重复累加恢复统计。
   - Chunk/Finalize/Accept/FinalAck 等普遍使用 `..` 忽略 job id；单流 ACK `:886`–`:894`、多流 ACK `:1192`–`:1195` 只让 outstanding 减一，不核对已发块身份，不实现重复 ACK 幂等。
   - 影响：授权对端可绕过单帧预算消耗内存；跨会话/乱序/重复语义缺少严格拒绝。现有整体哈希闸门仍保护诚实接收方的最终字节完整性，不宜描述成已经确认的哈希绕过。
   - 建议：预算按已协商清单规模绑定，页序号严格递增，索引唯一且在清单范围内，prefix 不超过实际块数，ACK 按待确认集合去重；保持总体文件数量可扩展时使用分页持久化，而非无界 `Vec`。

4. **P2：断点状态与并发任务身份缺乏隔离。**

   - `job_id_for`（`crates/ak-engine/src/lib.rs:366`）由源内容身份生成，没有 peer、收发方向、接收目录。
   - sender 无条件写该 ID 的记录（`:1273`–`:1277`）；receiver 使用远端 Offer ID 写记录（`:1712`）。
   - `crates/ak-store/src/lib.rs:187`–`:189` 仅按 job id 生成 `<job>.akjob` 路径。
   - 同一源并发发给多个对端、同机收发共享 checkpoint 目录，都会共享记录并相互覆盖。`serve_concurrently` 注释“每任务有独立 job id/记录”（`:1601`–`:1606`、`:1979`–`:1985`）没有被实现保证。
   - 建议：区分稳定内容 ID 与传输实例/记录 ID，记录键包含身份、方向及目标；对同一实例加互斥。临时文件名本身带进程和 sequence（store`:204`–`:212`），因此不是简单临时文件同名冲突，而是最终记录隔离问题。
   - 同路径 `.akpart` 共享问题及已提交文件重试失败已与 storage 审核者交叉确认，应由其主结论展开，避免重复。

5. **P2：协议文档中的持久确认顺序与实现不一致。**

   - `docs/PROTOCOL.zh-CN.md` 要求“写临时文件→可靠持久化→记录检查点→确认”。
   - 实际 `ak-storage::write_chunk`（`:699`–`:703`）只 `write_all`，无 sync。
   - 单流接收在 engine`:2201` 发 ChunkAck，之后每 16 块才 `store.save`（`:2224`–`:2228`）；多流同样先 ACK（`:2494`）再通过事件触发 checkpoint（`:2377`–`:2381`）。
   - **最终完成顺序是正确的**：storage`:734`–`:740` 整体摘要校验、`sync_all`，之后改名；engine`:1838`–`:1862` 完成所有 sink 后保存 `completed`，随后发送 FinalAck。
   - 重启时 `.akpart` 重新哈希可避免直接信任过期 checkpoint，但这并不等于按文档提供了“每块 ACK 表示持久保存”的承诺。建议明确 ACK 的持久语义，必要时批量 sync 后再 ACK/checkpoint，并测试掉电/故障注入。
   - PROTOCOL 还写“线缆格式未冻结”，wire 顶部则明确“frame v1 frozen”，应同步为当前规范。

6. **路径重授权的跨层确认。**

   - 引擎仅在 send`:1284` 与 receive_job`:1620` 调用 `gate_channel`；`:449`–`:456` 使用 `channel.path()`。
   - 之后发送和接收循环没有每次路径变更重授权。与主审发现的 `DirectChannel.path` 缓存及未禁 QUIC migration 组合，构成 INV-004 缺口。
   - `crates/ak-itests/tests/t08_reconnect_recheck.rs:6`–`:8` **明确承认不覆盖 connection migration**。该测试实际覆盖断线、新端口、新连接 TLS 钉扎及恢复；不能将其作为存活连接迁移已验证的依据，但测试自己的文字并未夸大。

7. **wire 的正面结论与两个兼容性缺口。**

   - `Frame::decode`（`:377`–`:410`）确实在 payload 分配前检查版本、类型、类型预算及全帧硬上限；帧外尾随与 payload 内尾随均检查。
   - `Cursor::take`（`:675`）使用 `checked_add`；字符串预算和 UTF-8/Cc 校验在 String 分配前完成（`:735`）；digest、entry、resume 向量预分配同时受声明数及实际剩余字节约束（`:771`、`:824`、`:869`）。u64 以大端字节传输，没有浮点精度问题。
   - **未知摘要算法被静默改成 BLAKE3**：`:768` 读取任意 `digest_alg`；`:197`–`:207` 转换时丢掉该字段；`ak-core::FileEntry::new`（`manifest.rs:102`）写死 BLAKE3，导致 storage 的未知算法拒绝检查失效。建议显式验证并保留算法标识。
   - **默认单文件有 64 GiB 边界**：engine`:114` / wire`:154` 限每条目 65,536 个摘要，默认块为 1 MiB（`ak-core/src/handle.rs:80`），所以 64 GiB+1B 会在 wire`:771` 被拒。Manifest 分页仅按文件条目拆页（engine`:562`），不会拆单文件摘要向量。增加块尺寸也仅推迟边界；ResumeRequest 的文件数量分页没有解决这一点。建议单文件摘要分页/树状摘要，并明确当前能力边界。
   - `encode`（`:346`–`:367`、`:526`–`:542`）仅 `debug_assert` 后 `as` 转换，生产环境缺少可返回错误的编码长度校验；当前来自网络的 decode 数据已有预算，这主要是本地 builder/API 可靠性缺口，不是已确认远程内存安全漏洞。

抽查的测试断言质量：

| 测试 | 有效断言 | 实际覆盖范围及缺口 |
|---|---|---|
| `ak-wire::rejects_oversized_declared_payload`，`:1534` | 空/短 payload 时精确断言 `Oversized`，验证类型预算优先于“缺 payload”；另测固定 Accept 16B 及全帧上限 | 高质量单帧负例；不覆盖 Offer 字段产生的下游分配和跨帧累计 |
| `ak-wire::rejects_trailing_bytes_inside_and_after_payload`，`:1589` | 独立制造帧外与帧内各多 1B，均精确匹配 `Trailing(1)` | 高质量，确实覆盖两层尾随检查 |
| `ak-engine::pause_holds_sender_until_resume`，`:3672` | 800ms 收不到新帧；Paused 到 Resumed 事件区间零 ChunkSent；恢复后恰好 8 块 | 确实验证暂停负行为，但仅 `window=1`、Fake receiver、单流，不是 GUI 暂停或多流/大窗口验证 |
| `ak-engine::multistream_loopback_transfer_is_byte_identical`，`:3804` | 三文件字节一致、空目录存在、文件流发送事件交错、进度事件有实际字节值 | 不是只检查 Result::Ok；但总量约 60 KiB，无法发现超时控制流问题 |
| `t08_reconnect_recheck` | 旧端口有界失败、新端口不同、job id 不变、恢复块数≥1、指纹相同、最终目录逐字节一致 | 真实 QUIC 环回重连测试；未验证 live migration，文件头已如实声明 |

所有上述“测试质量”结论来自阅读断言，**本轮未重跑这些测试**。

---

确认存在取消未收敛的问题，建议定为 **P2**，准确范围是取消状态提前发布、监听与部分多流子任务暂时存活；不能说取消后会无限传输或由后台任务完成最终提交。

- `crates/ak-ffi/src/core.rs:1123` 调用 `entry.handle.abort()`，随后 `:1128`–`:1138` 立即发布 `cancelled by caller`，没有等待任务真正退出。Tokio abort 是安排取消，不是同步等待清理完成。
- `crates/ak-net/src/lib.rs:621` 的 `DirectChannel` 没有 `Drop` 实现，只有显式 `close()`（`:659`）。我读取了 `Cargo.lock` 锁定的 **quinn 0.11.12** 本机依赖源；其 `connection.rs:292`–`:295` 明确规定，必须所有 Connection 和各类 stream 引用都被释放才自动关闭连接；`RecvStream` 和 `SendStream` 都持有 `ConnectionRef`。因此丢掉外层 `DirectChannel` 无法立即关闭仍由子任务持有的连接。
- `serve_concurrently` 在 `crates/ak-engine/src/lib.rs:2013` 丢弃接受任务的 JoinHandle。取消外层任务后，接受任务仍拥有 endpoint，并继续当前 `accept_within`；直到有连接到达或超时，向已关闭的 `channel_tx` 发送才退出。默认剩余时间最多约 30 秒，因此取消后立即用同端口重新开始接收可能遇到地址占用。这里的超时由 `ak-net/src/lib.rs:510` 明确约束，不是永久残留监听。
- 多流文件任务由 `ak-engine/src/lib.rs:2465` 独立 spawn，拥有 `DirectStream` 与 `LocalFileSink`。父任务退出后，它可以继续当前一轮接收、`:2491` 写块及 `:2494` ACK，随后在 `:2507`–`:2515` 向已经关闭的事件通道发送时发现失败并结束。**父 `receive_job` 已被实际销毁之后，每个这种子任务最多继续当前一轮块处理；不能声称它会无限写入。** `job_cancel()` 返回到父任务实际退出之间还有调度延迟，因此 API 返回时也没有立即静止保证。
- 多流控制读取任务 `:2298` 同样分离，会持续到下一个控制消息或 idle 超时。
- 最终 `sink.finish()`、`completed` 保存、FinalAck 位于父 `receive_job` 的 `:1838`–`:1862`，这些分离的文件子任务没有最终提交调用。因此没有证据支持“被取消后分离任务仍能独立完成全部提交”。正在执行的同步文件操作也不能被 Tokio abort 立即打断，应将这个一般性的取消时序边界与分离子任务问题区分开。

建议让监听、控制流和数据流子任务属于同一个受管理的取消域，保存 JoinHandle/使用 JoinSet，取消时显式关闭连接、停止接受并等待子任务退出，最后才发布稳定的 `cancelled` 结果。适合的验证是“取消返回并报告终态后端口能立即复用，且 `.akpart` 大小不再变化”；本次没有新增实验或修改文件。