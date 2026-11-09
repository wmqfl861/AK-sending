# Wave-36 修复轮独立审核报告（提交 624b523）

审核日期：2026-11-09。审核人：独立审核员（与修复执行者不同的第三方，用户指定）。审核对象：提交 `624b5230ad79fd7b7e6864fd8aaf8983122fb5c5`（"fix: wave 36 - close all 13 findings …"，wave-36 全项目 Codex 终审 13 项发现的修复轮）。审核依据：上游报告 `reports/codex-full-review-2026-11-09.md`（R36-P1-01~05、R36-P2-01~08，基线 `37a57c0`）。

审核方式：逐文件通读 `git show 624b523` 全量 diff（ak-signal 三个文件、ak-session client.rs、ak-cli task_cli.rs/daemon.rs、ak-ffi task.rs、Flutter 服务/UI/测试、docs、jniLibs 二进制）；对每项发现对照报告的文件:行号与修复建议逐条核对；实跑全部指定验证命令；抽查新增测试的真实性与既有测试的期望值改动。审核过程未修改任何源代码、未执行 git commit。

---

## 一、逐项判定表

| 发现 | 判定 | 摘要（证据见下节） |
|---|---|---|
| R36-P1-01 CLI 任务码等待链无保活 | **已正确修复** | 共享 keeper 组件接入 CLI 四命令 + daemon + FFI；真实 65s 空闲 e2e |
| R36-P1-02 重连耗尽停留 Connecting | **已正确修复** | 批耗尽发布 Disconnected；退避有界不自旋、不放弃；黑洞恢复闭环 |
| R36-P1-03 失败握手泄漏配额 | **已正确修复** | RAII guard 覆盖全部提前退出含 abort；32 任务 reject 预算；超额 accept 即断 |
| R36-P1-04 stdin EOF/错误视为同意 | **已正确修复** | 三态 StdinLine；EOF/Io/截断一律拒绝；仅真实换行 Enter/y/yes 接受 |
| R36-P1-05 旧 signal 无界连接与阻塞 | **已正确修复** | 256 上限、15s/30s 帧期限、非阻塞 outbox、写预算、writer abort-on-close |
| R36-P2-01 遗留不可取消 stdin 任务 | **已正确修复** | 会话级单 detached 输入线程；时间戳门丢弃过期答案；线程可退出 |
| R36-P2-02 进度失败计数被健康事件清零 | **已正确修复** | drainProgress 显式结果；双通道均净才清零；事件异常 tick 直接 return |
| R36-P2-03 engineFailed 不达 GUI | **已正确修复** | 可回放 engineHealth 流 → 真实 adapter 转发 → 主页 engine_failed_card；in-flight/等待即时终止 |
| R36-P2-04 同身份多实例互踢 | **部分修复** | 连接锁只覆盖 CLI task 四命令；daemon/FFI(GUI) 不参与；服务器无 replaced 区分 |
| R36-P2-05 journal 回收后重放被判冲突 | **已正确修复** | v2→v3 增列迁移原子可重入；识别前置；revision=1 重建；四类差异拒绝 |
| R36-P2-06 冻结断言 6 与常量 30 冲突 | **已正确修复** | 断言同步 30；31 次边界 + 重连/撤码不重置的真实服务器测试 |
| R36-P2-07 Android 打包缺源码绑定门槛 | **未修** | 提交内 scripts/ 与 gradle 零改动；仅刷新了 .so 二进制（缓解当前实例，不修流程缺口） |
| R36-P2-08 文档漂移 | **已正确修复**（有一处内部矛盾残留） | 七份文档 + SHA 同步；但 ffi-v2.md 新段落自称 P1-01/02、P2-04"未修"与本提交实现矛盾 |

计数：**已正确修复 10 项、部分修复 1 项（P2-04）、未修 1 项（P2-07）、修复基本到位但留文档残留 1 项（P2-08）**。

---

## 二、重点深查明细

### R36-P1-02：reconnect 失败批次终态（深查结论：真修复）

`crates/ak-session/src/pairing/client.rs`：

- **所有失败路径都发布 Disconnected 了吗？是。** 三条路径全覆盖：(1) reader 自身断连 `on_disconnect`（client.rs:388-400）→ `state_tx.send_replace(Disconnected)`；(2) `reconnect()` 整批失败 → 在持有 lifecycle 锁的状态下发布 Disconnected（client.rs:1036-1038，注释明确 close() 无法插入、发布无竞态）；(3) 黑洞恢复原语 `drop_connection()`（client.rs:1049-1055）→ `force_disconnect()`（client.rs:1085-1092）在 connected 标志真实翻转时发布 Disconnected。成功路径发布 Connected(epoch)（:1448）。不再存在"停在 Connecting"的出口。
- **退避会不会自旋？不会。** 批间退避 `KEEPER_RETRY_BACKOFF_STEPS = [0,1,2,4,8,30]s`（client.rs 约 :1215），`keeper_retry_pause(n)` 在 n≥5 饱和于 30s；pause=0 仅第一批（一次普通闪断立即恢复，且批内 `reconnect` 自带 1→30s 封顶的尝试间退避），不可能出现连续零间隔重试。最坏周期 ≈ 批内 7s + 批间 30s，有界。
- **会不会永久放弃？不会。** `connection_keeper_loop`（client.rs:1246-1309）仅在三种情况退出：`ClientState::Closed`、shutdown watch 翻转、client 内部 state_tx 消失（changed() Err）。任何其他重连错误 `Err(_) => failed_batches += 1` 后 `continue` 重新评估。循环顶部按**当前值**（非仅变迁）决策，避免"掉线发生在首次 poll 之前、无新变迁可等"的永久卡死——这是对 watcher 驱动 keeper 的一个真实且必要的加固。
- 暂停窗口内他人重连成功不 churn（pause 后复查 Connected）；连接级 ping 错误不计入黑洞预算（只有 Timeout 计），3 次连续 Timeout → `drop_connection` → 顶层走重连，代际由 epoch 保护。
- 测试真实有效：`reconnect_batch_failure_ends_disconnected_not_connecting` 用**真实 serve_v2 registry**（进程内起真服务器再 drop），断言新订阅者看到 Disconnected 而非 Connecting，两轮批均验证。

### R36-P1-03：RAII guard 覆盖与 reject 预算（深查结论：真修复）

`crates/ak-signal/src/server_v2/engine.rs`：

- **全部提前退出路径覆盖，含 select 取消/abort。** `ConnSlotGuard` 在 accept 分支内创建后随 `slot` move 进 `tokio::spawn` 的 future（`let _slot = slot;`）——TLS 失败、TLS 30s 超时、ALPN 失败、`connection_loop` 正常返回均经 guard `Drop` 归还 `conn_count` 并从 `conn_tasks` 移除；**任务被 abort 时 future 被 drop，Drop 同样运行**（tokio abort 语义），取消路径被结构性覆盖，而非依赖手工补调。
- **reject 预算语义正确。** 超过 256 活动上限的连接：立即 `fetch_sub` 归还 live 计数（live 计数只属于 admitted 连接），进入独立 `MAX_REJECT_TASKS=32` 预算：预算内 spawn 礼貌拒绝任务（完成 TLS→ALPN→一条带 `FRAME_IO_DEADLINE` 写超时的 ServerError→带 `TEARDOWN_GRACE` 的优雅关闭），任务自身由 `ConnSlotGuard::reject` 归还 reject 计数并进 `conn_tasks`（shutdown 可 abort）；**预算外直接在 accept 处 drop socket，不 spawn 任何任务**。总任务上界 = 256 live + 32 reject，"live cap finally bounds all work" 的提交声明与实现一致。
- 残留（无害、已文档化）：`register_conn_task` 的 `is_finished()` 检查与插入之间存在极窄竞态，可留一个已结束的 handle 到 shutdown sweep——abort 已结束任务是 no-op，源码注释如实承认。记为信息项，不扣分。
- 测试真实有效：`failed_handshakes_release_their_admission_quota` 打满两轮各 256 个失败握手（立即关闭 + 注入垃圾字节），随后合法客户端以 10s deadline 轮询重试断言重新准入（修复缺失时该测试必然超时 panic，非恒真）；`over_cap_flood_beyond_the_reject_budget_is_dropped_immediately` 用 256 条持有 TLS + 32 条静默 socket 钉满两级预算，断言第 257+33 条**立即**收到 EOF 而非等待 TLS 超时。

### R36-P1-04 / P2-01：consent 三态 + 单输入线程（深查结论：真修复）

`crates/ak-cli/src/task_cli.rs`：

- **三态与判定**：`StdinLine::{Line{text,complete}, Eof, Io}`；`consent_decision()`：Eof/Io/`complete=false`（EOF 前最后一段无换行的截断输入）一律 false；仅 `complete` 且 trim 后为 ""/y/yes 才 true。原 `unwrap_or_default()` + `read_stdin_line()` 空串映射 true 的路径已删除（diff 中两段旧代码整体移除）。n/no 与无法识别的答案拒绝并打印原因。
- **时间戳机制正确性**：`TerminalInput::answer()`（task_cli.rs 新增）先取 `started`，再 `try_recv` 循环清空已排队旧输入；等待窗口内收到的行若 `line.at < started`（读取完成于本提示开始之前、仅投递滞后）同样丢弃后**继续等待**。两层防御覆盖了"旧行在 drain 与 recv 之间才入队"的竞态；`at` 由 reader 线程在 `read_line` 返回时刻打戳，语义正确。提示开始瞬间（started 之后、drain 之前）入队的新输入会被 drain 丢弃——视为提示前杂散输入，边界合理。
- **线程真的会退出**：单一 detached OS 线程（非 tokio spawn_blocking）跑 `read_loop`：会话结束 receiver drop → `blocking_send` 失败 → 线程 return；EOF/Io 各投递一次后线程自然结束；线程 spawn 失败时 sender 随闭包 drop、channel 报 closed、每个提示 fail-closed 拒绝。选择 detached 线程的理由（阻塞 read_line 不可取消、blocking-pool 任务会拖住 runtime 关闭）在注释中成立——原报告 P2-01 指出的"main 返回后 runtime 等待阻塞任务"问题（main.rs:3768）被该架构绕开，无需改 main.rs。
- 测试：`terminal_input_discards_expired_prompts_answers` 构造两条带过期时间戳的 y 行，断言下一提示等满窗口拒绝、且排空后真实 Enter 仍能接受——针对"旧答案消费新提示"的核心断言真实有效；`consent_verdicts_pin_the_stdin_contract` 覆盖 Enter/y/Y/yes/n/no/杂散/EOF/Io/截断全部边界。

### R36-P1-05：旧 signal 语义零回归抽查（深查结论：真修复、无回归）

`crates/ak-signal/src/server.rs`：

- 五个维度全部落实：256 接入上限（超限 accept 处直接 drop，不 spawn——旧平面无 TLS，直接断开即礼貌）；15s 首帧 / 30s 稳态帧 I/O 双期限（超时 `break` 断开而非继续读，半帧不会 desync）；outbox 64 深 `try_send` 非阻塞投递，满/关队列判定为停滞消费者并 `close.notify_one()` 主动关闭；主 accept/sweep 循环的通知 flush 改为同步 `flush()`，**不再 await 任何客户端队列**（报告指出的主循环停顿点）；writer 每条写带 30s 预算、失速主动 close，收尾 `join_or_abort`（250ms 宽限后显式 abort，替换原先"timeout 后丢弃 JoinHandle"的泄漏写法）。
- **既有测试零期望改动**：diff 对旧测试的触碰仅 3 处 `handle_connect(...).await` → `handle_connect(...)`（函数同步化的机械适配，断言不变）；lib 单元套件 164 passed（含全部旧 signal 测试）。`process`/`deliver_one` 同步化后 Welcome/ServerError/Cancel 的投递语义不变（队列从不满 64 时 try_send 与 send 等价）。
- 新增 4 个测试均为行为测试（静默连接 15s 被断、认证后静默 30s 被断、256+1 超限立即断开且配额随关闭归还、writer 写失速上报 close、满队列 flush 丢消息并关闭消费者），有失败模式（超时 panic），非恒真。

### R36-P1-01：CLI/daemon/FFI keeper 接线（深查结论：真修复）

- 四个 task 命令共享 `open_session`（task_cli.rs:1920/1998/2056/2103 四个调用点），会话栈内 `spawn_connection_keeper`，teardown 中 `keeper_stop.send(true)` + abort（finish 流程完整）。daemon（daemon.rs:1441 起）与 FFI（task.rs:1316）的旧内联 keeper 均替换为同一共享组件。
- 保活预算论证正确：20s ping 严格落在服务器 30s FRAME_IO_DEADLINE 之内（`keeper_constants_pin_the_registry_silence_budget` 冻结）。
- **65s e2e 是真的**：`keeper_keeps_an_idle_code_owner_online_past_the_silence_cut` 以真实 `serve_v2` fixture 起 owner，`tokio::time::sleep(65s)` 真实等待（默认非 paused 时钟）后 joiner 仍配对成功并校验 blake3——ak-cli 测试套件总耗时 67.24s 与之吻合，证明该测试真实执行而非跳过。

### FFI keeper 换公共组件（集成者手改部分，深查结论：合格）

`crates/ak-ffi/src/task.rs:1290-1319`：

- **stop 后 watcher 泄漏？不泄漏。** 新增的 1s `interval` watcher 轮询 `rt.is_stopped()`：stop 翻转 → `keeper_stop_tx.send(true)` → watcher 自身 break 退出（≤1s 延迟）；runtime 被 drop 时任务随 runtime 结束。watcher 持有 stop watch 的 Sender，watcher 退出即 drop → keeper 的 `shutdown.changed()` 返回 Err → keeper 退出——即使 watcher 意外早退，keeper 也有第二条退出路径（ClientState::Closed）。
- **abort_tasks 覆盖？覆盖。** keeper handle 仍存于 `stack.keeper`（task.rs:1317-1319），`abort_tasks`（task.rs:493-504，wave-33 既有代码）abort keeper；watcher 本身不在 abort 列表，但靠 is_stopped 轮询收敛，最多多活 1s。记为信息项（非泄漏，只是协作式而非即时）。
- daemon 侧 keeper 直接复用 daemon 共享 shutdown watch，退出语义与原内联版本一致且更强（原来无 ping、无批间退避）。

### R36-P2-04：连接锁（深查结论：部分修复）

`crates/ak-session` 无关；实现在 `crates/ak-cli/src/task_cli.rs`（CONNECTION_LOCK_FILE/`acquire_connection_lock`/`spawn_connection_lock_heartbeat`/`Drop`）：

- **同进程**：holder.pid == 本进程 pid 不拒绝、覆写刷新（同进程顺序运行场景，测试覆盖）。
- **陈旧锁**：心跳 5s 刷新、15s 判陈旧；崩溃持有者的锁可破，破锁后文件写本进程 pid。
- **删锁竞态**：`Drop` 先读文件确认仍属本进程才删除——进程 A 挂起被 B 破锁后退出时不会误删 B 的锁（测试 `connection_lock_breaks_stale_and_own_locks_and_releases` 覆盖）。
- **缺口（部分修复的依据）**：(a) 锁协议参与方只有 CLI task 四命令——全仓库 grep `acquire_connection_lock`/`CONNECTION_LOCK_FILE` 仅 task_cli.rs 命中；**daemon 建立同身份常驻控制连接但既不写锁也不读锁**（daemon 与 CLI task 默认同一 `~/.ak-sending` 身份目录），CLI task 启动时看不到 daemon 的锁，daemon×CLI task 并存互踢窗口原样存在；FFI/GUI 侧同样无锁（GUI 默认身份目录在 AppData 支持目录，与 CLI 天然隔离，属缓解而非机制）。(b) 服务器对同 device_id 新 Hello 仍是直接关闭旧连接（engine.rs 无 replaced 原因区分），keeper 对"被替换"与"普通断网"采取同一立即抢回策略——报告建议的三个组成部分只落地了"启动时拒绝第二实例"一条。锁自身的 read-check-write 获取非原子（两进程同时启动可双双获锁）与"挂起恢复后 heartbeat 覆写他人锁"两个边缘见新发现 N2/N3。
- commit message "a second instance errors out instead of mutual kicking" 对 CLI task 成立、对 daemon/GUI 不成立，正文未声明范围，判部分修复。

### R36-P2-05：v2→v3 迁移与重放（深查结论：真修复）

`crates/ak-signal/src/server_v2/store.rs`：

- **原子性/可重入**：`migrate_v2_to_v3` 单 `Immediate` 事务内逐列 `pragma_table_info` 存在性检查 + `ALTER TABLE ADD COLUMN`（幂等），末尾统一 stamp `user_version=3` 后 commit；v1 库走 v1→v2（显式 stamp 2）→ v2→v3 链。SQLite 事务性 DDL 保证中途崩溃整体回滚；`interrupted_v2_to_v3_migration_resumes_and_completes` 用两种 partial 态（仅一列 / 两列但未 stamp）验证重开即补齐。
- **revision=1 重建**：pre-v3 行 `claim_response` 为 NULL，重建响应硬编码 `revision: 1`（claim 插入恒为 1，行当前 revision 可能已被 revoke 提到 2）——`claim_retry_after_journal_recycle_replays_the_original_result` 与 v2 迁移测试都先 revoke 再重试，专断言 revision==1。
- **四类差异拒绝**：canonical body 含 lookup/ttl/cap/endpoint，`claim_bodies_equivalent` 折算 36B/41B 默认尾后不等即 `RequestConflict`，测试逐一覆盖 ttl/cap/endpoint/lookup 四种变体。
- **顺序**：`prior_claim_replay` 在 quota（active/小时计数）与 `lookup_taken` 检查**之前**（store.rs claim 流程 hunk），`recycled_claim_retry_is_recognized_before_quota_checks` 验证 30/小时打满后旧请求重试仍 Replay、真新请求仍 RATE_LIMITED。重放后 re-journal 回快路径。pre-v3 行 body 由不可变列重建（ttl = (expires-created)/1000）与写入侧 `expires = created + ttl*1000` 自洽。

### R36-P2-02 / P2-03：双通道清零与 engineHealth（深查结论：真修复）

- **P2-02**：`drainProgress` 返回 `ProgressDrainResult{polled, malformed}`，`clean = polled && malformed == 0`；`_pumpOnce` 事件通道 `AkFfiException` → `_notePollFailure` 后**立即 return**（不跑 drain、不清零），进度通道异常/StateError → `polled=false` → 不清零；仅双通道全净 tick 清零。原 0→1→0 循环的两个方向（事件健康救进度、进度干净救事件）都被封死。测试用 `failPollProgressCalls` 模拟"事件健康 + 进度持续原生失败"，断言第 10 次 tick 闩上且 reason 带原生细节；另有全净重置与 drainProgress 显式结果测试。
- **P2-03 链路完整**：服务层 `engineHealth` = `Stream.multi`（先接活 tap 再发当前快照，FIFO 无缝）；UI 契约（`ui/service/simple_transfer_service.dart`）新增 `EngineHealth`/`engineHealth` 抽象；真实 adapter `FfiSimpleTransferService.engineHealth` 转发（`.map` 惰性、每次 getter 新管线、不破坏 per-listen 快照重放，测试验证迟到订阅者收到闩上状态）；`UnusableSimpleTransferService` 空实现；HomePage 订阅 → `_engineHealth.isEngineFailed` 时渲染 `engine_failed_card`（与启动错误卡共用 EngineErrorCard 样式）；`_ensureLive` 闩门使 in-flight 操作在下一个 poll tick 以同一 `StateError('传输引擎故障…')` 终止（1 天超时的接收等待被闩而非超时打断，测试证明）；`_fail` 在闩后不再改写 reason/phase。widget 测试断言健康时无卡、闩后 `engine_failed_card`/`engine_failed_card_detail`/原因原文/重启提示可见且页面保持可用——"engine_failed_card 真的出现在主页"由 `home_flow_test.dart` 真实锁定。

### R36-P2-06 / P2-08

- **P2-06**：config.rs 冻结断言 6→30（附上调理由注释）；engine.rs `hourly_claim_budget_boundary_is_thirty_and_survives_reconnect_and_revoke` 在真服务器上验证 30 成功、第 31 次 RATE_LIMITED、重连（新连接同身份）与撤码均不重置——完全对应报告修复建议。
- **P2-08**：README/HANDOFF/ROADMAP（wave-29~35 收口史 + SHA）、SECURITY（30/小时 + claim/redeem 两组预算的明确区分 + "重连不绕过全部限速"边界）、pairing-flow §11（已实现值 vs 建议值分列）、storage-schema（v2→v3 DDL 与语义）、ffi-v2 §8.5（keeper 20s/30s 依据、C07/C12 边界）均已同步。残留一处自相矛盾：ffi-v2.md 新增段落写"R36-P1-02 未修；R36-P1-01 未修；R36-P2-04 未修"——该三行成稿早于同提交内的修复落地，交接者按文档会误判当前状态（见新发现 N4）。

### R36-P2-07：未修（对照提交声明）

`git diff 37a57c0..624b523 --stat -- scripts/ flutter_app/android/` 为**空**：`package_windows.ps1`（-IncludeAndroid 失败仅告警继续带旧库）、`build_android_ffi.sh`（冻结导出检查不含 ak_task_*）、`flutter_app/android/app/build.gradle.kts`（消费暂存库无源码绑定）全部原样。提交内唯一相关动作是刷新两个 jniLibs `.so`（arm64-v8a 9,154,328B / x86_64 9,142,208B）——这消除了"当前产物落后于 wave-35/36 修复"的**实例**，但报告要求的**流程门槛**（ffiBuilt 结果与 Rust 源修订入 MANIFEST、失败不继续携带旧库、交付模式强制成功刷新、APK 实装校验）一项未落地，下次构建仍可复现"显示包含 Android 实则携带旧库"。判定：未修（仅实例缓解）。

---

## 三、新发现问题（分级）

| # | 级别 | 问题 | 定位 | 说明 |
|---|---|---|---|---|
| N1 | **中** | P2-04 锁协议参与方不全，daemon×CLI task 互踢窗口仍在 | crates/ak-cli/src/daemon.rs:1441 起（无锁调用）；task_cli.rs 锁实现 | daemon 用与 CLI task 相同的默认身份目录 `~/.ak-sending` 持有常驻控制连接，但不写锁文件；CLI task 的 acquire 检查不到它。两者并存时服务器逐个替换连接、两个 keeper 各自立即抢回，即报告 P2-04 的原始互踢场景在"GUI 关闭、daemon + CLI task"组合下仍可复现。属部分修复的残留而非回归。 |
| N2 | 低 | 连接锁 heartbeat 覆写不校验持有者 | task_cli.rs `spawn_connection_lock_heartbeat`/`write_lock_file` | 持有者 A 挂起 >15s 被 B 破锁后若恢复（睡眠唤醒典型场景），A 存活的 heartbeat 任务会在 5s 内把锁文件覆写回 A 的 pid，后续进程误判持有者；此时 A、B 实际并存（互踢根源已存在），覆写只污染后续判定。read-check-write 获取也非原子。代码注释自认 best-effort，但此两边缘未点名。 |
| N3 | 低 | 提交标题宣称"close all 13 findings"与事实不符 | 提交 624b523 message 首行 | 13 项中 P2-07 未修（scripts 零改动）、P2-04 部分修复；实际为 10 项完整关闭 + P2-08 关闭（带文档残留）+ P2-04 部分 + P2-07 未动。正文（分 agent 列表）未虚报、逐条与 diff 相符，"Verified: workspace 1154/0 + clippy + contracts 5/5; dart 161/161, flutter 173/173" 经本审核实跑全部吻合；但标题口径构成对上游"全部关闭"的错误声明。 |
| N4 | 低 | ffi-v2.md 与同一提交实现自相矛盾 | docs/contracts/ffi-v2.md §8.5 新增段 | 段内写"一批重连全部失败后停留 Connecting…（R36-P1-02，未修）""ak-cli task 等待链没有等价 keeper（R36-P1-01，未修）""同设备多实例互踢（R36-P2-04，未修）"——三项恰在本提交内修复/部分修复。文档时点错位未回收。 |
| N5 | 信息 | register_conn_task 竞态留 finished handle 至 shutdown | engine.rs（注释已自认） | abort 已结束任务为 no-op，无资源影响。 |
| N6 | 信息 | FFI stop-watcher 为 1s 轮询协作退出，不在 abort_tasks 列表 | ak-ffi/src/task.rs:1304-1313 | stop 后 ≤1s 收敛，runtime drop 即终，非泄漏；仅时序备注。 |

回归风险抽查（既有测试是否被悄悄改期望）：ak-signal 三文件（engine/store/server）diff 中**零断言删除**；client.rs/task_cli.rs 零断言删除；server.rs 旧测试仅 3 处 `.await` 移除（同步化机械适配）。Flutter 侧唯一期望值改动为 `simple_transfer_service_test.dart:1116`——C12 闩后 `createCode` 由"成功但 phase 不遮蔽"改为"抛出同一传输引擎故障 StateError"，这是 P2-03 latch gate 的**更严格**语义适配（新行为阻止对已判定故障的引擎继续发起操作），非削弱。未发现恒真断言或测试绕行（TerminalInput 测试经 from_receiver 注入通道，真实 cover 判定与时间戳门逻辑；65s e2e 与两个洪水测试均为真实时钟/真实服务器）。

---

## 四、验证复核输出（本审核实跑，2026-11-09）

| 命令 | 结果 |
|---|---|
| `cargo test -p ak-signal --features server-persistence` | 全绿：lib **164 passed**（含旧 signal 全部既有+新增测试）、tests/client_v2 **26**、tests/protocol_v2_codec **52**、tests/server_v2 **26**、doc-test 1；0 failed |
| `cargo test -p ak-cli -p ak-session -p ak-ffi` | 全绿：ak-cli **144**（67.24s，含真实 65s idle e2e）、ak-session **51**、ak-ffi 各套 **77/29/24/13** 等合计；0 failed |
| `cargo test --workspace` | **1154 passed / 0 failed** —— 与提交声明 1154/0 完全一致 |
| `cargo clippy --workspace --all-targets` | **0 warning / 0 error** —— 与声明一致 |
| `flutter test`（flutter_app） | **All tests passed! +173** —— 与声明 173/173 一致 |
| `dart test`（flutter_app/packages/ak_ffi_dart） | **All tests passed! +161** —— 与声明 161/161 一致 |
| `python scripts/check_contracts.py` | **RESULT: 5/5 checks PASS** —— 与声明一致 |

环境备注：flutter/dart 不在 PATH，使用 `C:\Users\Administrator\flutter-sdk\bin` 下 SDK 执行。提交中"server redeploy deferred（SSH banner 失败）"无法在本审核中验证，属披露性声明，方向诚实（明确说明线上服务器仍是上一波二进制）。

---

## 五、总结论

### **有条件通过（13 项中：完整修复 10、部分修复 1、未修 1、修复到位带文档残留 1）**

**理由**：

1. **修复质量高**。五个 P1 与 P2-01/02/03/05/06 的实现经逐行深查与真实测试验证均判定为正确修复：RAII 对 abort/取消路径的结构性覆盖、reconnect 终态发布的无竞比设计、keeper 有界退避（不自旋不放弃）、stdin 三态判定与时间戳门、双通道清零、revision=1 重建与原子可重入迁移，都有会真实失败的测试锁定（65s 真实空闲 e2e、512 失败握手洪水、中断迁移恢复等），且既有测试零削弱。全部六组验证命令实跑结果与提交声明逐项吻合。

2. **但"close all 13 findings"不成立**。R36-P2-07（Android 打包源码绑定门槛）在本提交中**零代码改动**，仅以刷新 `.so` 二进制缓解当前实例；R36-P2-04 仅 CLI task 侧落地，daemon/FFI 不参与锁协议、服务器无 replaced 语义，daemon×CLI task 互踢窗口仍在。提交正文本身诚实，标题口径错误（N3）。

3. **放行条件**（不满足则应视为不通过终审口径）：
   - 承认并登记 P2-07 为未关闭：补打包门槛（ffiBuilt/源码修订入 MANIFEST、失败不携带旧库、ak_task_* 导出集合校验），或在交付文档中显式豁免并说明现有风险；
   - P2-04 补齐 daemon（以及后续 FFI/GUI）参与锁协议或明确文档化"CLI task 与 daemon 并存仍互踢"的边界；
   - 修正 ffi-v2.md §8.5 中"P1-01/P1-02/P2-04 未修"的自相矛盾表述（N4），并回写 README/HANDOFF/ROADMAP 中 wave-36"进行中、工作树未提交"的过时状态。

以上三项均为流程/文档/范围收口动作，不阻塞已落地修复的技术正确性；在完成前，本提交的正确表述是"关闭 13 项中的 10 项 + 1 项部分 + 1 项文档同步"，而非全部关闭。

---

*本报告由独立审核员生成于 2026-11-09；审核过程只读（唯一写入为本报告文件），未修改源代码、未执行 git commit。*
