# 交接文档：批次 H（2026-11-13，复核工单 H0–H3 执行完毕）

> 工单来源：[REVIEW_2026-11-13-batchG](REVIEW_2026-11-13-batchG.zh-CN.md) 第 8 节（Claude 对批次 G 的复核，PR #14）。
> 执行方式：H0 由集成者完成；H1/H2/H3 三个并行 agent（文件域互不相交 + 两处共享文件交叉协调）；集成者逐项复核、独立复验门禁、按"一项一提交"入库。
> 分支 `dev/review-fixes-20261111`；基线 `2e213ea`（H0）之前为批次 G 收官状态。

## 0. 一页结论

- **7 个编号全部处置完毕**：CR-P1-07（P1）、CR-P1-08（P1）、CR-P2-15（P2）、CR-P3-11/12/13/14（P3）→ 状态表全部"已完成"。
- **GUI 路径与 CLI 路径现在走同一打洞代码**（CR-P1-07 的结构性目标）：配对时 opener + 15s 保活 + 收养 socket 连通测试，FFI 与 CLI 调用同一 `sender_pairing_opener`——矩阵验证 CLI 即验证 GUI 行为。
- **job_done/job_started 事件序从"调度运气"变为"构造保证"**（CR-P2-15）：同步钩子在槽位释放前触发，200/200 压测 + 隔离树 10 连跑零失败。
- **诚实边界**（不声称未验证的事）：矩阵三个新案例（late-send-5s/40s、recv-no-lowttl）与 netem 256MiB 新口径需 Linux CI/特权环境跑出证据；Windows `IP_TTL` 行为本机单测已真实验证，真机确认留给 C4；netem 8MiB 收益对照**待测**（口径已按复核 §5.2 更正，不再声称"实证"）。
- CI：H0 修复链 `2e213ea`（红）→ `a604acc` 绿（run 43404654677）；批次 H 主体推送 `aae55db` → run 43410016551（结果见 §4）。

## 1. 提交清单（时间序，本批新增）

| 提交 | 项 | 内容一句话 |
|---|---|---|
| `2e213ea` | H0/CR-P3-11 | fail_case/sampler 实体化 + 状态表 commit 前缀门槛 + G6 占位更正（文档+脚本） |
| `a604acc` | H0/CR-P3-11 | validator 单测同步（2e213ea 的 CI 红修复）+ 新增 2 专测 + 死代码清理 |
| `33083f4` | H1/CR-P1-07+08 | 配对时 opener/保活、收养 socket 连通测试、GUI 不禁用、Windows setsockopt 低 TTL、矩阵三新案例（含 H3 的 RFC5737 网段半——共享工作树交叉入库） |
| `4fb2ad2` | H1 报告 | reports/h1-report.md（agent 交付报告） |
| `5bd4b9c` | H2/CR-P2-15 | on_attempt_settled 钩子 + JobTerminals 注册表：job_done 在槽位释放前打印 |
| `53ad204` | H3/CR-P3-12 | 契约补登：reflexive_opener 事件 + ServerError.code 0x1001 码表 + GUI 白名单（含 H1 的 batchH 语义修订——交叉入库，内容融合完整） |
| `792e367` | H3/CR-P3-14 | legacy-signal CI job + smoke_loopback 默认构建兼容 + record-id 断言修复 |
| `aae55db` | H3/CR-P3-13 | RFC5737 矩阵网段收尾 + IPv6 SKIP + 200ms 断言收紧（含 task_net 孪生）+ netem 256MiB/RTT + 8MiB 措辞更正 |
| `84b58d9` | H3/CR-P3-13 | netem runner 随 256MiB 改名同步（`--exact` 过滤器+校验器+fixture）+ 三角防漂移守卫（详见 §4） |
| `6235bec` | H3/CR-P3-13 | 双栈跳过改用 raw errno（E0599 修复：不存在的 ErrorKind 变体名只在 cfg(unix) 编译路径暴露）——详见 §4 |
| `4f23127` | H0 docs | 状态表七行回写 + CR-P2-04"8MiB 窗口实证"措辞更正 + 本交接文档（其 netem 二跑暴露 §4 第 3 项） |
| `6302487` | H3/CR-P3-13 | 校验器接受基线行字面 "-" RTTs 列（netem 二跑红修复，忠实重构验证纪律）——netem 三跑成功 |

批次 I（本交接之后追加，非批次 H 范畴，均已 CI 绿）：`e498fa2`（I2）unix clippy 硬化——c_char 转换债清零 + `scripts/clippy_unix_probe.sh` 本地预警（run 43418204512 绿）；`2dd2507`（I1）tls.rs 窗口覆写 + quinn stats 钩子 + bench A/B 臂——A/B 首测 run 43420288043 **成功**（CI run 43420288223 绿），8 MiB vs 1.25 MiB 对照数字在 run summary/artifact。

## 2. 关键实现与根因（复核要点对照）

### CR-P1-07（GUI 打洞时序，复核 §4.4 改法 1/2）
- **根因**：GUI 配对后连通测试用临时 socket（端口受限锥形 NAT 下结构性必败）→ 假 unreachable 禁用发送按钮；用户选文件晚于接收端 4s 打洞窗口。
- **修复**：① `task_net::sender_pairing_opener` 共享核心——opener 在**配对完成**时跑（`SENDER_DIAL_PAUSE` 后），随后每 15s 一发探测形状保活（同目标集、收养 socket），直到 send 开始/连接结束/10min 预算；每个保活同时刷新发送端正向流和接收端打洞映射（反向应答式）。② 连通测试改在收养拨号 socket 的 `try_clone` 上（`probe_dial_round_on`，join/owner 两路径；接收端保持配对面探测——quinn 已收养其 socket）。③ GUI：仅 `disconnected` 结束页面活性；unreachable 文案角色相关（"仍可尝试发送/仍可等待对方发送"）；按钮保持可用。
- **语义修订入库**（ffi-v2 §8.1）：`reflexive_opener` 改为配对时恰好一条、保活静默无事件；`reachable:false` 不再是数据面判决。
- **矩阵验证面**：`AK_EXP_SEND_DELAY_MS`（5s/40s 档）模拟"配对后迟发送"，`AK_EXP_NO_TTL` 模拟"无低 TTL 能力主机"——均为隐藏 debug 开关、默认关，仅矩阵设置。

### CR-P1-08（Windows 低 TTL，复核改法 3b）
- `SocketTtlGuard`：无逐包 TTL 控制消息的主机以 `setsockopt(IP_TTL)` 在**阶段期间**调低 socket 自身 TTL、阶段末恢复（含 Drop 路径）。安全性由构造保证：配对时刻该 socket 上尚无任何 QUIC 连接。setsockopt 拒绝 → fail-open 退回正常 TTL 降级（本 Windows 主机单测真实验证三分支）。
- 两种低 TTL 均不可用时**整轮打洞推迟 `PUNCH_LOW_TTL_PHASE`**，让发送端配对时 opener 的低 TTL 包先占窗口——实现复核建议 3b 的排序而**无需 wire 能力位**（不动 `pairing/task.rs`，不碰冻结协议）。
- opener 的 IPv4 包在可发低 TTL 时一律走低 TTL：opener 目的只是打开本侧 NAT 流，抵达对端从来不是目的——正常 TTL 的提前到达会污染对端 NAT（run-43304434327 的 inbound-only 记录）。

### CR-P2-15（事件序）
- 见状态表行与 `5bd4b9c` 提交说明。要点：actor 是 scheduler core 唯一写者 → 钩子在 `on_finished` 内、终结转换前同步执行 = 顺序由构造保证。新生命周期测试在钩子内**同步排空事件流**断言 `TaskFinished`/后继 `TaskAdmitted` 均未先到。daemon 侧预分配任务 id（0xD1 标记字节命名空间，唯一性单测）先注册后提交，消除注册竞态；fallback 保证恰好一次打印。

### CR-P3-11/12/13/14
- H0：`fail_case` 在 683f79 被描述但从未写入——本批实体化；拨号期 conntrack 采样器每 5s 一条。门槛扩展抓"G6 提交"类占位；**教训**：2e213ea 扩门槛却没跑 validator 自身单测（本地只跑了主逻辑）→ CI 红（占位 fixture 触发两错误而断言为一），`a604acc` 修复并补 2 个专测（G6 提交占位拒/带注记哈希过）。
- H3/CR-P3-12：三处补登全部通过 check_contracts（5/5）。**共享工作树交叉入库说明**：H1 与 H3 同时编辑 ffi-v2.md §8.1 与 nat_matrix_test.sh——53ad204 扫入了 H1 的语义修订段（内容已核：融合完整、无冲突），33083f4 扫入了 H3 的 RFC5737 网段半（aae55db 收尾其余）。两个文件的最终状态都是双 agent 工作的完整并集，提交归属与内容贡献已在状态表行注明。
- H3/CR-P3-13：RFC5737 化的动机——旧 10/8 核心网段被 `prefers_lan` 判为 LAN，反射优先/短公网预算路径在矩阵中从未触发；家用 LAN 故意保持 RFC1918（模拟 NAT 私侧）。早退断言两处收紧至 200ms（task_net 孪生由集成者补——H3 点名的遗留，实测 ~20ms，5/5 绿）。
- H3/CR-P3-14：smoke 的 wire-job-id 断言与 `record_id_for` 冻结设计冲突（"no wire frame ever carries this value"）在当前 HEAD **必败**——改为断言 `<32hex> receive/send completed` 行形态；默认/feature 双构建实测 PASS。

## 3. 验证证据（本机 + CI）

- 本机（Windows）：fmt 全仓干净；clippy `--workspace --all-targets -D warnings` 干净；ak-session 226/0（H2 时点）、ak-cli 144/0、ak-net 46/46、ak-signal legacy 202/202、validator 39/39、check_contracts 5/5、check_repository 零错；同码串行隔离树 10×绿（此前 flaky 项）；200ms 收紧断言 5/5；flutter（H1 agent）：analyze 干净、test 173/173。
- H2 隔离复验（detached worktree `5bd4b9c`，排除并行 WIP 干扰）：clippy 干净、全测试绿；首次后台跑 send_admission 1 例失败（机器高负载时序抖动），复跑 6/6 + 同码串行 10×全绿，判定环境抖动而非回归（CI 会再证）。
- CI：`a604acc` 绿（run 43404654677，全部 11 job）。批次 H 主体 `aae55db` → run 43410016551（§4 填结果）。

## 4. CI 结果（aae55db 的两次红 → 84b58d9/6235bec 修复 → tip 复验）

**两次失败，均已根因定位并一振修复：**

1. **主 CI run 43410016551**（aae55db）：唯一红 job = Rust ubuntu 的 **Clippy** 步骤。根因：H3 的双栈跳过分支用了**不存在的** `ErrorKind::AddrfamilyNotSupported/ProtocolNotSupported` 变体（E0599）——该代码在 `#[cfg(unix)]` 测试里，Windows（本机+CI）从不编译它所以全绿，ubuntu clippy `--all-targets` 是该路径首次被编译。修复 `6235bec`：改匹配 `libc::EAFNOSUPPORT/EPROTONOSUPPORT` raw errno + `Unsupported` kind。**用 NDK 交叉探针验证**（`cargo clippy --target aarch64-linux-android`——该 target `cfg(unix)` 为真，unix 分支全部编译）：ak-net/ak-session/ak-cli `--all-targets -D warnings` 全干净。探针顺带暴露 ak-ffi 测试 helper 两处 wave-33 旧代码的 `as u8` 恒等转换（aarch64 上 c_char=u8 触发、windows/x86_64-linux 上 c_char=i8 合法）——**非 CI 问题**（没有任何 CI job 对 aarch64-linux 跑 clippy），本批之前即存在，未动。
   其余 11 job 在 aae55db 已全绿，含：**NAT 矩阵（RFC5737 网段 + 三个新案例 late-send-5s/40s、recv-no-lowttl 全部 pass-v4）**、**legacy-signal 首跑**、Flutter 173+、Repository checks 40 行门槛。
2. **netem-benchmark run 43410016378**（aae55db）：`aae55db` 把 bench 测试改名为 `..._256mib_...`，runner 仍以 `--exact` 调旧名 → cargo 选中零个测试 → runner 按其"NOT-MEASURED 即失败"设计正确判红（不静默跳过）。契约测试当时全绿是因为常量/mock/脚本三方一起漂移（自洽但脱离 bench 源）。修复 `84b58d9`：`--exact` 过滤器、日志校验器（256MiB 前缀+5 列行正则+RTTs 列）、fixture 三方同步 + 新增**三角防漂移守卫**（常量↔runner↔bench 源码，未来改名在 CI step 5 就失败）。40/40 契约测试绿。
3. **netem 二跑 run 43413205487**（4f23127）：名字已对、测量真实执行，但校验器仍拒——bench 基线行 RTTs 列打印字面 **`"-"`**（无整形、wall/RTT 无定义），84b58d9 的正则只收 `{number}|inf`。修复 `6302487`：RTTs 备选扩为 `{number}|-|inf`，fixture 基线行同步镜像真实输出；**验证纪律升级**——把校验器 heredoc 从 runner 抽出、对照"按 bench println 格式串逐行重构的完整真实输出"（表格三行+主机计数行+参考段）本地实跑通过后才推送。

**最终结果**：
- 主 CI：`4f23127` → run **43413205516 全绿**（12/12 job，含 NAT 矩阵三新案例、legacy-signal 首跑、Flutter 173+、40 行状态表门槛、三平台 portability）。
- netem-benchmark：`6302487` → run **43415588553 成功**——**256 MiB 新口径实测证据落地**（基线/RTT≈100ms/RTT≈200ms 三档 + RTT 数列 + lo/UDP 主机计数；具体数字见该 run 的 step summary 与 `netem-benchmark-6302487-*` artifact）。CR-P2-04 的 8 MiB 收益对照自此有了测量基座（A/B 覆写入口见 §5）。
- 三振规则：主 CI clippy gate 一振修复（6235bec）；netem gate 两振（84b58d9 名字漂移、6302487 校验器行形）——不同断言、各自根因独立，均已修复并有提交说明可溯。

## 5. 未完成 / 待所有者 / 下一步

- **已闭环**：主 CI tip 全绿（run 43413205516）；netem 256MiB 新口径实测落地（run 43415588553 成功，256 MiB/RTT 列/主机计数证据在 run summary 与 artifact）。
- **批次 I（后续工程，已在途）**：quinn stats 钩子 + tls.rs 窗口覆写入口（`AK_EXP_QUIC_WINDOW_KIB`）+ bench A/B 臂——CR-P2-04 的 8 MiB 收益对照自此可测（对照 run 43415588553 的默认窗基线）。I2：unix 路径 clippy 硬化（`scripts/clippy_unix_probe.sh` 本地预警 + c_char 转换债清零）已入库。
- **C4 真机清单**（复核修订版前置全部满足后）：两台真实设备跨 NAT 互传；Windows 真机 `IP_TTL` 行为确认（本机单测已验证，真机网络栈下再证一次）；GUI 配对→选文件→迟发送实测（矩阵已模拟 5s/40s 档）。
- **待所有者**：E5 签名密钥；E6 服务器部署（OPERATIONS §0 + 防火墙关 TCP 7100；CR-P2-13 完成前不停旧进程）；E7 LICENSE；CR-P3-03 域名；CR-P3-05 SSH 密钥。
- **C4 真机清单**（复核修订版前置全部满足后）：两台真实设备跨 NAT 互传；Windows 真机 `IP_TTL` 行为确认（本机单测已验证，真机网络栈下再证一次）；GUI 配对→选文件→迟发送实测（矩阵已模拟 5s/40s 档）。
- **待所有者**：E5 签名密钥；E6 服务器部署（OPERATIONS §0 + 防火墙关 TCP 7100；CR-P2-13 完成前不停旧进程）；E7 LICENSE；CR-P3-03 域名；CR-P3-05 SSH 密钥。
- **建议下一批**（若有）：quinn `Connection::stats()` 的 ak-net/ak-engine 统计钩子 + tls.rs 窗口覆写入口（netem A/B 的两个前置）；矩阵案例若 CI 时长吃紧可拆分 job。

## 6. 红线遵守

- 未访问/部署/改动生产服务器（122.51.121.24）；无签名密钥；无真实 IP/凭据/用户路径入库（RFC5737/回环/文档段only）。
- 无 relay/TURN；deny.toml bans 扫描通过（CI deny job 绿）。
- 契约冻结流程遵守：所有契约变更走 doc-first（ffi-v2/protocol-v2 先行或同提交），check_contracts 5/5。
- 模拟 ≠ 真机：矩阵结论只声称模拟证据；C4 真机步骤保留给所有者。
- 未动 main、未 force push、未合 PR；全部提交在 `dev/review-fixes-20261111`。
- 三振规则：本批 `checks` gate 首振（2e213ea validator 单测）即振内修复（a604acc），无二振。
