# H1 交付报告：CR-P1-07 + CR-P1-08（GUI 打洞时序 + Windows 接收端低 TTL）

> 执行者：ZCode（H1 独占 `task_net.rs` 与两处适配层；依据复核 `docs/REVIEW_2026-11-13-batchG.zh-CN.md` §4/§8）。
> 提交：**`33083f4`**（`fix(CR-P1-07+08): pairing-time opener/keepalive, adopted-socket join probe, Windows low-TTL punch`，分支 `dev/review-fixes-20261111`）。
> 契约修订（ffi-v2 §8.1 `reflexive_opener` 时机 + CR-P1-07 段）因共享工作树被并行批次的提交卷入 **`53ad204`**（H3/CR-P3-12），内容已在库中，特此说明。

## 1. 改了什么（对照复核 §4.4 改法 1–4）

### 改法 1：配对完成时打洞 + 保活（核心进 task_net，FFI 与 CLI 同一函数）

- **`crates/ak-session/src/pairing/task_net.rs`** 新增共享核心 `sender_pairing_opener`：
  - 前半 = 原 `sender_reflexive_opener`（`SENDER_DIAL_PAUSE` 停顿后从收养 socket 向对端全部非 loopback 候选发探测形状包，时序与 CLI 逐字节一致）；
  - 后半 = 保活：每 `OPENER_KEEPALIVE_INTERVAL`（15 s，30 s 未应答超时的一半）从同一收养 socket 的只发克隆向同一目标集发一包，直到停止位置位（发送开始/连接断开/teardown）、或 `OPENER_KEEPALIVE_BUDGET`（10 min，与任务码 TTL 默认一致）耗尽。停止检测粒度 250 ms（`sleep_polling_stop`）。
- **`crates/ak-ffi/src/task.rs`**：`spawn_sender_opener` 在两个配对完成入口（`run_join` 的 sender 分支 + `owner_side_probe` 的 sender 分支）spawn 上述共享函数；任务登记进 `spawned`（teardown 可中止）；`reflexive_opener` 事件改为配对完成时恰好一条（opener 半场的包数）。`run_send` 删除发送期 opener，改为起点处置位收养 socket 的 `punch_stop`（停止保活）。
- **`crates/ak-cli/src/task_cli.rs`**：`send_phase` 调用同一共享函数（opener 行仍打印同一措辞；新增一条有界保活结束摘要行；`AbortOnDrop` 保证 Ctrl-C 时中止保活任务）。

### 改法 2：连通测试改用收养 socket；GUI 不再因测试失败禁用发送

- **task_net** 新增 `round_probe_outcome`：把 `probe_dial_round_on`（发送阶段已在用的探测轮）的观测折叠回 `ProbeOutcome` 词汇（有观测=Success，静默=每候选一条有界理由）。
- **ak-ffi** 新增 `adopted_socket_probe`：sender 的配对时连通测试跑在收养拨号 socket 的 `try_clone` 上（等待 `SENDER_DIAL_PAUSE` 让 opener 包保持"收养 socket 首个出站"次序；无收养 socket 时回退一次性 socket 轮）。`run_join` 与 `owner_side_probe` 的 sender 分支改用之；**接收端保持配对面探测**（其 QUIC socket 已被 quinn 收养，克隆上收包会与 quinn 抢 socket 队列）。`probe_result`/`task_join` 结果形状不变。
- **`flutter_app/lib/ui/pages/connection_detail_page.dart`**：`live` 只由 `disconnected` 决定（`unreachable` 不再判死）；文案按角色改为"连通测试未通过，仍可尝试发送。"（发送侧）/"连通测试未通过，仍可等待对方发送。"（接收侧）；选择/发送按钮保持可用。

### 改法 3（CR-P1-08）：Windows 接收端低 TTL

- **task_net** 新增私有 `SocketTtlGuard`：无逐包 TTL 的主机在低 TTL 阶段临时 `setsockopt(IP_TTL)`（std 的 `ttl()`/`set_ttl()`）调低整个 socket、阶段结束恢复（含早停路径的 Drop 恢复与失败重试）。配对时刻该 socket 上尚无任何 QUIC 连接，符合复核 §4.4/§6.4 的更正；选项被拒绝时 fail-open 回退旧的全窗口正常 TTL。
- **无需能力位（复核 3b 的替代实现）**：当主机完全没有低 TTL 发送能力（逐包与 socket 级都不可用，或矩阵开关）时，`punch_candidates` 把**整轮打洞推迟 `PUNCH_LOW_TTL_PHASE`（1 s）**——发送端自己的低 TTL opener（见下）在该窗口内先占本侧 NAT 流，本侧正常 TTL 打洞包只会命中已存在的出站流（反向匹配→REPLIED），不再制造入站独有的 conntrack 记录。**不碰 endpoint blob 线格式**（`pairing/task.rs` 不在本次所有权内，也未加契约面）。
- **opener 的 IPv4 包改为低 TTL**（能发低 TTL 的主机）：opener 的职责只是打开本侧 NAT 流，抵达对端从来不是目的；正常 TTL 的 opener 若先于对端首个打洞包到达，会在**对端** NAT 上制造同样的入站独有记录（run-43304434327 的镜像）。IPv6 目标保持正常跳限；本环回/局域网投递不递减 TTL，不受影响。

### 改法 4：矩阵新增两类案例（隐藏调试参数，默认关闭）

- **`scripts/nat_matrix_test.sh`**：`run_case`/`case_pass` 增加可选 sender/receiver 环境变量前缀；新增三案（均 pcone↔pcone、期望 `pass-v4`）：
  - `late-send-5s`（`AK_EXP_SEND_DELAY_MS=5000`，发送侧）
  - `late-send-40s`（`AK_EXP_SEND_DELAY_MS=40000`，覆盖 30 s 未应答超时）
  - `recv-no-lowttl`（`AK_EXP_NO_TTL=1`，接收侧关闭低 TTL 阶段 = 修复前 Windows 形态）
- 两个开关都是隐藏调试参数（环境变量读取，默认关闭，无 CLI 面）：`AK_EXP_SEND_DELAY_MS` 在 `task_cli.rs::exp_send_delay`，`AK_EXP_NO_TTL` 在 `task_net.rs::exp_no_low_ttl`。矩阵脚本默认运行不设置它们。
- **注意**：矩阵脚本是 Linux 网络命名空间专属（本 Windows 主机按脚本自身的 skip 规则诚实跳过），三案的实际 `pass-v4` ×10 连跑需在 CI nat-matrix / Linux 主机执行——本轮未能本机执行，见 §3 未验证事项。

## 2. 验证（本机 Windows，实际执行）

| 门槛 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | 0 警告 |
| `cargo test --workspace --no-fail-fast --locked` | 全绿（含 ak-session 144、ak-ffi 61+29+24、ak-cli 132+13 等；一次 FFI 用例竞态见下） |
| `RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps` | 0 错误 |
| `python scripts/check_contracts.py` | 5/5 PASS |
| `python scripts/check_repository.py` | 无 failure/error |
| `flutter analyze` | No issues found |
| `flutter test` | **173/173 全部通过**（含新增 widget 测试） |
| `bash -n scripts/nat_matrix_test.sh` | 语法通过 |
| 本机端到端 CLI 冒烟（真实 registry + 真实 QUIC，loopback） | `AK_EXP_SEND_DELAY_MS=2000` 的 `join-send` 成功：日志含 `reflexive opener sent 7 packet(s)`、`experimental send delay 2000 ms`、`pairing keepalive held the mapping: 0 tick(s)… ended at send start`（2 s < 15 s 首拍，故 0 拍——符合设计）、`connected via`、`done:`；sha256 双端一致 |

新增测试钉：

- **task_net**（43 项全过，其中 socket 级 TTL 回退在本 Windows 主机是**真实路径**而非模拟）：`socket_ttl_guard_arms_restores_and_restores_on_drop`、`sleep_polling_stop_wakes_on_deadline_and_on_the_flag`、`pairing_opener_runs_then_keepalives_until_stopped`、`pairing_opener_keepalive_ends_at_the_budget`、`round_probe_outcome_folds_heads_and_silence`；punch 测试扩展了 TTL 恢复断言与 `low_ttl_socket`/`start_delayed` 报告字段三分支。
- **ak-ffi**：`reflexive_opener_event_fires_at_pairing_on_both_sender_paths`（join-sender 与 owner-sender 两个入口）。
- **widget**：`unreachable keeps the send buttons enabled and says the send can still be tried (CR-P1-07)`（按钮 onPressed 非空 + 新文案 + 发送流程仍走通 + "连接未就绪"提示不再出现）、`receiver unreachable copy waits for the peer (CR-P1-07)`；同步更新 `direct_unavailable_guidance_test.dart` 中钉死旧行为（按钮禁用）的用例为新行为。

修掉的一个**既有测试竞态**（`declined_offer_keeps_the_listener_and_done_converges_both_sides`）：改法 2 让 join op 更快返回（早退探测轮 ~0.5 s 取代 3.3 s 的重试探测），暴露了该用例一直隐含的假设——owner 侧 1.5 s 探测窗口此刻仍在跑（`probing`）。已改为轮询 `ak_task_list` 等探测落定再断言（`probing` 是瞬态，不是判决）。

## 3. 未验证 / 诚实边界

1. **矩阵三新案未实际运行**：需要 Linux + root 的网络命名环境（本机 Windows，脚本自身会 SKIP）。CI 的 nat-matrix 任务落地后需按复核 §7.1 验收（每案连续 10 次 `pass-v4`）。**尤其 `recv-no-lowttl` 案是对"opener 低 TTL + 打洞推迟"设计的中路径证明**，逻辑推演见 §1 改法 3，但 iptables/conntrack 层面的实证未做。
2. **Windows 双栈 socket 的 `IP_TTL` 行为**：复核指出需实测"双栈 socket 发往 IPv4 映射地址时 IP_TTL 是否生效"。本机单测用 AF_INET socket 真实验证了 guard 的 arm/restore；产品路径的 AF_INET6 双栈 socket 上若 `setsockopt` 被拒，代码会 fail-open 回退（打洞推迟路径兜底），不破坏功能——但"Windows 接收端低 TTL 真正生效"要等 C4 真机或 CI Windows 任务实测确认。
3. **真机（C4）**：按复核 §7，C4 仍需等 H1+H2 完成并独立复核后再做。
4. 保活使发送端 NAT 出站流保持 UNREPLIED→REPLIED 的跃迁依赖对端 NAT 的 conntrack 语义（Linux 型 30 s/120 s）；真实家用路由器行为有差异——15 s 间隔已按复核建议留足余量，真机验证在 C4。

## 4. 所有权与并行说明

- 只改了所有权内文件：`task_net.rs`、`ak-ffi/src/task.rs`、`ak-cli/src/task_cli.rs`、`connection_detail_page.dart`、`nat_matrix_test.sh`、`docs/contracts/ffi-v2.md`；另按验证要求**追加**了 `flutter_app/test/connections_test.dart` 与 `direct_unavailable_guidance_test.dart` 的用例（新文案/按钮状态的 widget 测试是本轮验收项）。未触碰 `daemon.rs`、`ak-net/src/lib.rs`（工作树中 ak-net/bench_netem/STATUS/TEST_PLAN 的改动来自并行的 H0/H2/H3 批次，非本批）。
- 全部门槛是在含并行批次改动的合并工作树上跑的（即当前分支 HEAD 状态）。
