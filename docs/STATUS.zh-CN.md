# 项目状态

> 更新：2026-11-13。本页是"当前状态"的**唯一来源**：[README](../README.md) 与 [HANDOFF](HANDOFF.zh-CN.md) 只链接本页，不再各自维护状态叙述；历史波次不在此重复。
>
> 逐项修复进度以 [REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md) 第 7 节状态表为准，本页只记口径与结论。

## 当前状态（2026-11-13）

- **阶段＝审核修复轮已完成并通过独立验收复核（有条件）**。第一轮修复（`dev/review-fixes-20261111`，32 个提交，审核文档 [REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md) 全部 19 项编号处置完毕）由 Claude 独立复核：**通过（条件＝本轮 F0 文档收尾，见 [REVIEW_2026-11-12-acceptance](REVIEW_2026-11-12-acceptance.zh-CN.md)）**；新发现 8 项（P1×1/P2×4/P3×3）已补入审核文档第 4.5/7 节。
- **当前执行（2026-11-13）**：批次 G 接近收官（[批次 F 复核](REVIEW_2026-11-12-batchF.zh-CN.md)的更正已全部消化）。已完成：**G0** 矩阵加固（5 连绿验收：runs 43268998764–43284511782）；**G1/CR-P1-06** 低 TTL 打洞+反射优先交错拨号（strike-3 根因=opener 误滤 RFC1918 反射，修复后 pcone↔pcone **pass-v4 转绿**，10 连跑验收进行中）；**G2/F7** 探测早退+probe_observed 事件+GUI 诊断导出；**G3/F11** E3 直连失败文案；**G4/F5** 确认窗口 120s+UI 倒计时+90s 延迟同意验收测试；**G5/E1 切片** 旧明文 signal 特性隔离（legacy-plaintext-signal，默认构建不含）；**G6** R36-P2-04 双侧收官（服务端 0x1001 原因帧+客户端四方锁统一+N2 加固）+ ServerInfo 契约修订（0x0008 位）+ keeper 30s 暂停评估（维持，理由冻结）；**G8** netem 首批实测（220.4/24.7/12.7 MiB/s；2026-11-13 复核 §5.2 更正口径：这只证明窗口已不再是 1.25 MiB 默认值造成的瓶颈，8 MiB 的收益待测——两档均约 25–26 个 RTT 传完 64 MiB，8 MiB 生效应约 18 个；H3 起载荷 256 MiB、每档打印 RTT 数与 lo/UDP 计数）。计划外根因修复：revoke 连接战争根治（配速+原因帧——诊断证实配速此前因裸 EOF 从未生效）、daemon 启动可观测性（stderr 保留+快速失败+starting 事件）。10 连跑验收达成（全部 NAT job 绿）；交接文档 [HANDOFF-BATCH-G-2026-11-13](HANDOFF-BATCH-G-2026-11-13.zh-CN.md) 已入库。追加完成：G5b（旧 Dart 服务层删除）、P3-08（daemon 事件配对）、P3-10 遗留（状态表格式门槛）。G6b（GUI ServerInfo 显示）已入库（`7d64ebf`）。
- **当前执行（2026-11-13 晚）**：批次 H 完成（[批次 G 复核](REVIEW_2026-11-13-batchG.zh-CN.md)工单 H0-H3 全部入库，三路并行+集成复核）。**H0/CR-P3-11**（`2e213ea`+单测同步 `a604acc`，CI 绿 run 43404654677）：fail_case/sampler 实体化+状态表 commit 前缀门槛；**H1/CR-P1-07+08**（`33083f4`，两个 P1）：配对时 opener+15s 保活（FFI/CLI 同一共享函数，GUI 与矩阵测的是同一代码路径）、连通测试改收养 socket、GUI 不再据 unreachable 禁用发送、Windows 以 `setsockopt(IP_TTL)` 拿回低 TTL 打洞阶段、矩阵迟发送/无低 TTL 三新案例（Linux CI 验证）；**H2/CR-P2-15**（`5bd4b9c`）：`on_attempt_settled` 钩子使 job_done 在槽位释放前打印（结构性顺序，200/200 压测）；**H3/CR-P3-12/13/14**（`53ad204`/`aae55db`/`792e367`）：契约补登（reflexive_opener+0x1001 码表+GUI 白名单）、RFC5737 矩阵网段+环境健壮+netem 256MiB/RTT 口径更正、legacy-signal CI 门+smoke 修复。netem"8MiB 窗口实证"措辞已按复核 §5.2 全面更正（收益待测）。交接文档 [HANDOFF-BATCH-H-2026-11-13](HANDOFF-BATCH-H-2026-11-13.zh-CN.md)。
- **复核更正（2026-11-12 晚，Claude，见 [REVIEW_2026-11-12-batchF](REVIEW_2026-11-12-batchF.zh-CN.md)）**：symm 案的定性不成立——是矩阵的 IPv4 NAT 案没有隔离 IPv6，产品走了 IPv6 直连，不是模拟保真度问题（纯 IPv4 复跑时 symm↔symm 按预期失败）。纯 IPv4 下 **pcone↔pcone 失败**（conntrack 元组冲突改写了发送端外部端口），新编号 **CR-P1-06（P1）**；CR-P1-05 改判为"结构达标、验收未达成"；矩阵修复记为 CR-P2-14。下一棒为批次 G（先 G0 修矩阵，再 G1 修 CR-P1-06）。
- **复核更正（2026-11-13，Claude，见 [REVIEW_2026-11-13-batchG](REVIEW_2026-11-13-batchG.zh-CN.md)）**：CR-P1-06 的修复只在 CLI 路径生效（本机纯 IPv4 复跑通过，CI 10 连跑属实），**GUI 路径没有生效**——配对后的连通测试用临时 socket，在纯 IPv4 双锥形场景下必然失败，GUI 随即禁用发送；GUI 的发送又晚于接收端的低 TTL 阶段；Windows 接收端没有低 TTL 阶段。新编号 CR-P1-07、CR-P1-08（P1），另有 CR-P2-15（同码串行测试偶发失败）与 CR-P3-11 至 CR-P3-14。**C4 要等 CR-P1-07、CR-P1-08 修好后再做**，E6 可以先做。下一棒为批次 H。（2026-11-13 批次 H 补记：CR-P1-07/08 已修复入库 `33083f4`，C4 前置仅剩矩阵新案例 CI 绿与复核确认；Windows 双栈 `IP_TTL` 行为待真机确认。）
- **E1-E4 已由验收方决定**（E1 冻结旧模型：删旧 Dart 层随时做、旧 Rust 特性隔离须在 F3 之后；E2 确认窗口 30s→120s；E3 暂不上 UPnP、对称 NAT 文案已定稿；E4 删除 MANIFEST.sha256）。E5/E6/E7（签名密钥/服务器部署防火墙/LICENSE）待所有者，见验收文档第 6 节。
- **生产服务器**：122.51.121.24 常驻 registry 7200（TLS）＋旧 signal 7100/7101（2026-11-05 部署形态，线上二进制版本仍未核实）。本轮未访问、未部署、未改防火墙。**E6 注意：CR-P2-13（STUN 拆独立服务）完成前不得停旧进程，只能防火墙封 TCP 7100。**
- **客户端交付**：Windows GUI/CLI 已交付打包；Android debug APK 已构建（模拟器装机，非真机）；Linux GUI 未构建；macOS/iOS 所有者排除。
- **v1 任务码双入口**（GUI `ak_task_*` FFI + CLI `task` 命令族）均已接入；跨 NAT 直连能力＝接收端 IPv6 候选 + 同 socket STUN 反射 + 单向探测（完整双向打洞见 CR-P1-05/F2）。

## 最新验证（CI）

- Run [43121806432](https://github.com/wmqfl861/AK-sending/actions/runs/43121806432)（修复轮收官提交 `ae0b4e7`）：全绿——Rust ubuntu/windows、flutter（186+166）、android（双 ABI+符号校验+APK 上传）、audit、deny、checks、portability×3；release-deep-tests 手动任务按设计 skipped。
- 独立验收复核另在 Linux 容器（rustc 1.97.0、无 IPv6、网卡 192.0.2.2/24）实跑全量门槛：1,203 通过 / 0 失败 / 7 忽略（验收文档第 2 节）。
- 本地门槛命令清单见审核文档第 6 节；本页只记结论，不复述计数。

## 交接入口

> **交接文档**：第一轮 [HANDOFF-REVIEW-FIXES-2026-11](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md)；批次 F [HANDOFF-BATCH-F-2026-11-12](HANDOFF-BATCH-F-2026-11-12.zh-CN.md)；批次 G [HANDOFF-BATCH-G-2026-11-13](HANDOFF-BATCH-G-2026-11-13.zh-CN.md)；批次 H [HANDOFF-BATCH-H-2026-11-13](HANDOFF-BATCH-H-2026-11-13.zh-CN.md)。

- [HANDOFF](HANDOFF.zh-CN.md)：逐波交接记录、实现与验证边界、重启恢复手册（历史口径，其"当前/进行中"字样以本页为准）。
- [REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md)：本轮审核意见＋执行计划（第 5 节）＋状态表（第 7 节）＋待所有者决定清单（第 8 节）。
- [AGENTS](../AGENTS.md) / [CONTRIBUTING](../CONTRIBUTING.md)：开发规则、红线与门槛。
- [ARCHITECTURE](ARCHITECTURE.zh-CN.md)：模块职责与现状（2026-11-11 口径）。

## 历史去哪查

- [CHANGELOG](../CHANGELOG.md)：用户可感知的里程碑（Keep a Changelog 格式）。
- [BACKLOG](BACKLOG.zh-CN.md)：逐轮任务级完成状态与证据。
- [reports/](../reports/)：逐轮开发验证记录与历次审核报告（wave-28~31、wave-36 终审及其收口复核在内）。
- 更早的逐波叙述见 git 历史中的 README/HANDOFF 旧版本。
