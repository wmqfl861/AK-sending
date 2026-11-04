# AK-sending 邀请码功能终审（Codex / gpt-6-astra / reasoning=max，2026-11-05）

> 只读审核；覆盖邀请码全链安全/调度器/wire 准入/staging/集成测试质量/证据链一致性。

已完成静态只读审计；未修改文件，未运行测试、构建或服务。以下均为当前源码能够确定的事实，触发结果属于代码路径推导，并非本次运行复现。

**应优先进入总报告的发现**

1. **P1：文件接收仍存在符号链接 TOCTOU；staging 清理还可能越出接收根目录。**

   关键位置：

   - `crates/ak-storage/src/lib.rs:615`：`ensure_real_dirs` 依次 `create_dir` / `symlink_metadata`，只保留字符串路径，没有固定目录句柄。
   - `crates/ak-storage/src/lib.rs:1242`：检查 `.akpart` 不是符号链接。
   - `crates/ak-storage/src/lib.rs:1251`：随后用普通 `OpenOptions` 打开同一路径，没有 no-follow / reparse-point 防护。
   - `crates/ak-storage/src/lib.rs:732`：`verify_part_prefix` 会将不匹配的现有文件 `set_len(verified_bytes)`，通常可为 0。
   - `crates/ak-storage/src/lib.rs:1308`：`discard_staging` 仅对最终 `top` 做 `symlink_metadata`；父目录 `.ak-staging` 的链接不受检查，后续 `read_dir`、删除都按路径操作。

   可确定的触发：有接收目录写权限的并发本地进程，在校验与打开之间替换目录或 part 文件为符号链接，普通打开会跟随链接，随后可能截断根目录之外的文件。清理时将父 `.ak-staging` 替换为指向外部树的链接，`symlink_metadata(root/.ak-staging/<task>)` 仍可看到一个真实目录，遍历会删除外部树中的文件。仅对叶子 symlink 做检查不能阻止父链接，也不能消除检查后的竞态。

   修复建议：接收根目录、staging 根及子目录使用固定句柄；Unix 用 `openat`/`openat2` 与 no-follow/beneath 约束，Windows 用目录句柄、reparse point 拒绝与身份检查；创建、打开、提交和递归清理必须沿同一受限目录句柄进行。不要把 `ensure_real_dirs` 的现有注释“symlink-race check”作为竞态防护证明。

   现有测试 `staged_prepare_rejects_symlinked_staging_level`（2882）只覆盖**操作开始前已经存在**的链接，而且 `make_symlink` 失败会直接 `return`（2888–2889）；未覆盖交换竞态或清理中的父链接。

2. **P1/P2：准入模型与冻结契约严重脱节；接收端还允许 capability 降级绕过准入。应与“生产未接线”合并报告，不能声称当前 CLI 已遭远程认证绕过。**

   精确证据：

   - `crates/ak-engine/src/lib.rs:296` 的 `AdmissionCtx` 三个字段全部 `pub`，不具备契约要求的受控构造。
   - `crates/ak-engine/src/lib.rs:326` 的 `AdmissionDecider` 仅接收 `group: &[u8;32]`。
   - `crates/ak-engine/src/lib.rs:799` 解构 `TaskOpen` 时用 `..` 丢弃 `group_registry`；819 只调用 `decider(&group)`，没有真实 peer、pairing_id、task id、nonce 或 lease 等上下文。
   - `crates/ak-engine/src/lib.rs:2453` 至 2464：如果远端不宣告 `CAP_SCHEDULER`，即使调用的是 `receive_on_channel(..., decider, ...)`，仍将 `staged_task=None`，直接进入旧 Offer 流程，**整个 decider 不会执行**。2299–2302 的注释明确选择这种回退，与 ADR-004、scheduler 契约的“不支持则 PROTOCOL_UNSUPPORTED”冲突。
   - `crates/ak-wire/src/lib.rs:370`：实际 `TaskOpen` 没有 `pairing_id`、方向、attempt、admission_nonce；`group` 是 32 字节，而冻结 `code_id` 为 16 字节。
   - `crates/ak-engine/src/lib.rs:1902` 的 `send_on_channel` 也未检查 `channel.peer_fingerprint() == spec.pinned`。旧 `send` 在自己建立连接时完成的这一一致性条件，在注入通道后不再由引擎验证。报告与检查点仍使用 `spec.pinned`。

   根代理已核实生产 CLI/FFI 未接入这些入口；本审计 `rg` 的结果一致：除 engine 内测试外没有真实 `send_on_channel`/`receive_on_channel` 调用。应明确归类为“新准入安全契约未落地、公开接口仍不可安全接线”，而非“恶意远端可越过当前 CLI 的配对簿 TLS 身份校验”。

   修复建议：一次修完整契约与入口：用受控 `AuthorizedTaskContext` 绑定真实通道身份、本地 Ready relation、registry/code/pairing/transfer、方向、nonce 和有效 permit；接收新邀请码任务必须 fail closed；旧手工指纹入口单独显式选择 legacy；跨层 API 与 wire 同步后再接入生产 executor。

3. **P2：staging 内部名字未隔离，远端合法相对路径可进入清理命名空间，导致“成功后删除成功文件”。**

   证据：

   - `crates/ak-core/src/manifest.rs:97` 的 `validate_rel_path` 允许 `.ak-staging/<32hex>/file`；它只拒绝路径穿越、控制字符、Windows 保留名等。
   - `crates/ak-storage/src/lib.rs:1165` 没有增加 staging 保留命名空间检查。
   - `crates/ak-storage/src/lib.rs:1183` 按 manifest 的路径构建最终路径。
   - `crates/ak-engine/src/lib.rs:816` 对远端任务 ID 仅检查非零。
   - `crates/ak-engine/src/lib.rs:2845` 成功后无条件调用 `discard_staging(root, task)`。

   可确定的例子：远端使用 task ID `T`，提供最终路径 `.ak-staging/<T>/x`；暂存路径会在这个树下再嵌一层，提交后最终文件也在 `T` 的 staging 树内。引擎先保存 `completed`、发送 `FinalAck(verified=true)`（2788–2798），随后清理整个 `T` 树，把刚成功提交的文件删除。此路径无需本地符号链接权限。其他 task 的内部路径同样没有隔离。

   修复建议：将 staging 放到用户文件树之外的私有根，或在所有接收入口统一拒绝/映射保留顶层名字，并把任务目录绑定到经过认证的实例及独占所有权。不能仅假设发送者不会传 `.ak-staging`。

4. **P2：同目标同内容的并发提交未保证“一次提交、一次复用”；现有测试没有真正并发提交。**

   - `crates/ak-storage/src/lib.rs:891` 至 905：同内容复用仅发生在 `commit_no_replace` 前的存在性检查。
   - `crates/ak-storage/src/lib.rs:918` 至 925：真正原子提交返回 `AlreadyExists` 时，没有再次校验已有内容，直接按 `Fail` 报错或 `Rename` 另存。
   - 两个 `finish` 同时看到目标不存在后，一个提交成功，另一个会在 `Fail` 下失败，或 `Rename` 下多生成一份相同文件，与该模块及波次证据声称的并发复用语义不同。

   相关测试 `two_tasks_same_target_same_content_commit_once_and_reuse`（2637）有两个活跃 part，但 2670、2671 是**顺序** `sink_a.finish()`、`sink_b.finish()`；不同内容测试（2693）甚至在准备 B 前已完成 A（2706）。这些测试验证了顺序冲突处理与 part 隔离，不能证明提交竞争。

   修复建议：在原子 claim 的 `AlreadyExists` 分支重新核验已存在目标的安全句柄与摘要；同内容返回复用，否则继续 Fail/Rename。用受控 barrier 把两个线程卡在“目标不存在”之后再放行，并断言目标数与结果。

5. **P2：on-channel 任务依然提前扫描，且检查点实例隔离未接入 AKREC2。**

   - `crates/ak-engine/src/lib.rs:1909` 在策略 gate 和 admission 之前调用 `prepare_send`。
   - `crates/ak-engine/src/lib.rs:1841` 全量打开/扫描源，1850–1854 生成并保存检查点。
   - 直到 1981 才执行双端准入。因此“准入前不扫描、不生成完整 manifest”尚不成立；被拒绝的任务也先完成昂贵扫描。
   - 发送检查点在1850、接收检查点在2575都调用旧 `record_id_for`（505），其域仍为 `AKREC1`（512），不包含 `transfer_id`。两个相同内容、同 peer、同 root 的不同任务会共用检查点，而 staging 使用不同 task ID；新的队列 AKREC2 并不能自动修复引擎检查点。

   修复建议：拆开握手/准入与源扫描，让实际准入成功后才生成 manifest；新任务统一使用包含 transfer_id 的 AKREC2 record id，仅明确 legacy 恢复路径保留 AKREC1。生产接线完成前应避免宣称实例隔离已全链成立。

**另一个需披露的已知存储边界**

`crates/ak-storage/src/lib.rs:978` 至 982 在 FAT/exFAT 无 hard-link 支持时退回 `fs::rename`，能在竞争时覆盖另一任务刚创建的目标。962–966 的注释已承认该风险，因此不是未披露新发现，但与“atomic no-replace”的无条件承诺不相容。建议使用平台原生 no-replace 移动，或者明确返回不支持，不能保留覆盖式回退。

**wire 预算核查结果**

`ak-wire::payload_budget`（`crates/ak-wire/src/lib.rs:641`）与 `ak-net::type_budget`（`crates/ak-net/src/lib.rs:986`）的全部 15 个变体逐项相符；新消息的 payload 分别为：

| 消息 | wire | net | 冻结 scheduler 契约 |
|---|---:|---:|---:|
| TaskOpen | 96 | 96 | 101 |
| Admission / TaskAdmission | 34 | 34 | 59 |
| AdmissionAck / TaskAdmissionAck | 32 | 32 | 48 |

实际编解码布局的 96/34/32 与实际预算一致，没有发现新增帧导致网络预分配表漏项。`ak-net` 911–918 在分配 payload 前检查类型与预算；1023–1024 和 wire 都受全帧 cap 限制。真正问题是**实现布局与冻结契约不同**，不是两份预算表互相不一致。

**engine 关键测试抽查，可供报告直接采用**

- `on_channel_admission_granted_completes_full_round_trip`（engine 6694）确实通过真实 loopback QUIC 验证文件字节、报告字节数、decider 看到的 group 和完成后的 staging 清理（6727–6737）。但 decider 恒为 `Admit`、accept 恒为 true（6714–6722），不覆盖 Ready relation、registry、peer 或真实 scheduler permit。
- `on_channel_peer_without_scheduler_cap_is_protocol_unsupported`（engine 6891）正确验证**发送端**拒绝缺 capability 的接收端，且后续 wire 静默（6904–6916）。它没有验证反方向；接收 on-channel 降级缺陷因而漏检。
- `receive_on_channel_grant_waits_for_admission_ack`（engine 6986）正确验证不给 Ack 时失败且没有 staging（7024–7033）。但断言接受 `IdleTimeout` **或任意 `Stream` 错误**，只能证明没有成功推进，不能严格证明一定由指定 idle deadline 收敛，也没核验 tentative permit 释放。

**核实保留的既有边界**

- `gate_channel` 仍在发送 on-channel（1911）及接收公共内核（2434）执行。
- `DirectChannel` 字段不是公开可伪造状态；`DirectEndpoint::connect`（ak-net 501–506）从真实连接证书重新提取并核对指纹，`accept`（532–535）执行真实身份授权。
- 本次阅读到的数据入口与 wire 新消息没有把文件 payload 引入 `ak-signal`；不能因此声称已经完成抓包级“无中继”验证。
- 本次所有结论都是静态审核；没有重新运行现有测试。