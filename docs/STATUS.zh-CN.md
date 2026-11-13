# 项目状态

> 更新：2026-11-13（批次 J 进行中，J6 文档准备初稿；最终证据待补）。本页是"当前状态"的**唯一来源**：[README](../README.md) 与 [HANDOFF](HANDOFF.zh-CN.md) 只链接本页，不再各自维护状态叙述；历史波次不在此重复。
>
> 逐项修复进度以 [REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md) 第 7 节状态表为准，本页只记口径与结论。

## 当前状态（2026-11-13，批次 J 进行中）

- **阶段＝批次 J 实施与验收进行中**。此前批次 H/I 在 `97fac96` 上复核通过，附 1 项 P2、5 项 P3（[REVIEW_2026-11-13-batchH](REVIEW_2026-11-13-batchH.zh-CN.md)）；这是历史批次结论，项目层面的"通过审核"仍未达到。
- **J0、J4 已有对应提交的 CI 证据**：
  - J0：PR #15 已快进合入至 `0a17311`，CI run 43439835214 为 12 success / 1 skipped（手动 release-deep-tests）。
  - J4 / CR-P3-17：`aee55e0` 已推送，隔离树 clippy、ak-session、ak-cli、send_admission_cli 通过；CI run 43444609038 同为 12 success / 1 skipped。钩子按尝试报告结果与 terminal，daemon 仅在终态撤回注册项并打印；两次尝试的回归测试已记录在审核状态表。
  - 两个 run 的结论只适用于各自提交；不覆盖 J2/J3 在途工作或尚未形成的最终集成提交。
- **客户端 UI U1 / J1：进行中，尚未验收**。
  - U1 内容：实时传输进度、一次配对一次传输的正常收尾（CR-P2-16）、连接卡进度、内存"最近传输"、深色模式；契约 ffi-v2 §8.4 U1 修订。
  - J1 已完成旧/新 Windows DLL 与 Android 双 ABI 构建；Windows 第 1–5、10 项取得同机真实 DLL/CLI 行为证据。第 6 项清空已验证，另以相同配置/身份目录的两个真实进程补验：首进程注入合成历史且不清空，退出后次进程历史为空；两次直接退出码均0。注入只证明内存历史生命周期，不冒充新传输，原 pumpWidget 不作重启证据。
  - Windows 光栅输出仅见纯底色，第 7 项视觉走查受阻；integration_test 不代替可读性观察。Android 标准 debug APK 已构建安装，第 8/9 项继续验证；普通共享存储写入权限与系统 UI 响应问题尚在排查，未计为通过。
  - 逐项清单与结果由 J1 维护：[HANDOFF-CLIENT-UI-U1-2026-11-13](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)。构建、自动化行为、视觉走查与 C4 真机跨 NAT 分别验收。
- **J2 / CR-P3-15：代码及本地门禁通过，Linux十轮待验收**。Rust `c60512b`、UI `6e576d6`：单一配对生命周期gate管理活跃发送、绝对截止时间及唯一keeper；倒序失败、无资源到期、取消与UI终态过滤均有回归。最终本地门禁为Rust全仓/格式/clippy/rustdoc通过、契约5/5、Python94、Flutter208、Dart170。J5工具`fb19a01`已入库，最终CI与拒绝后重试连续10轮仍待实跑，不先判本项完成。
- **J3 / CR-P3-16：已完成于 `8ef4a07`**。精确独立快照完整本地门禁通过；CI run 43466913061 的12项常规任务成功、手动深测跳过，Android双ABI发布库无 AK_EXP_ 步骤实际执行成功；netem run 43466913049 成功，Windows默认DLL本地扫描无 AK_EXP_。后续集成版本另跑对应验证。
- **CR-P1-07、CR-P1-08：代码达标，验收进行中**。
  - H/I 复核时的三个新矩阵案例为 6/10，属历史时点；J0/J4 的两个 run 中 late-send-5s、late-send-40s、recv-no-lowttl 均 pass-v4，8 MiB byte-identical。
  - **J5 尚未执行**：待 J2/J3 最终提交固定后，在独立 CI 内对同一提交顺序跑 10 轮，包含上述三个案例与 J2 新增拒绝后 ≥40 秒重试案例，原有矩阵案例保持通过；不拼接历史 run 凑 10 次。
  - CR-P1-08 的 Windows IPv4 头 `IP_TTL` 抓包仍未取得，由所有者在 C4 执行；Windows 单测与 Linux 矩阵不能替代。
- **J6：文档准备中**。本页、审核 §7 与 [批次 J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) 正在同步；待最终证据到齐后按指定顺序核验，尚未作最终一致性确认。J7 / CR-P3-19 为可选 UDP 缓冲优化，**本轮不实现**，避免扩大变量。
- **C4：待 J1、J2、J5 实际完成后，由所有者执行**。内容包括两台真实设备跨 NAT 互传、Windows TTL 抓包、GUI 配对后迟发送、拒绝后重试与 U1 真实传输表现。E6 可并行准备，实际 C4 仍需所有者部署及版本/外部连通验证；不把 J1 同机或模拟器测试写成 C4。
- **E1–E4 已决定且各自工程动作有历史证据**：E1 特性隔离/旧 Dart 层删除（1c95d38、fc53613）；E2 确认窗口 120 s（aa7f3af）；E3 文案（e1ba8b1，UPnP 等仍暂不启用）；E4 删除 MANIFEST.sha256（951b44e）。旧 Rust 代码是否彻底删除按一个发布周期后再评估，不是 E6 后自动删除。
- **待所有者**：E5 签名密钥（正式命名/版本统一与签名验证仍见 CR-P2-02）；E6 服务器部署、防火墙及线上版本核实；E7 LICENSE；CR-P3-03 域名/钉扎轮换；CR-P3-05 SSH 密钥。E5、域名及 SSH 密钥事项不作为新增 C4 前置。
- **生产服务器**：
  - 文档历史记录为 registry 7200（TLS）＋旧 signal 7100/7101（2026-11-05 部署形态）；线上二进制版本和独立 STUN 迁移结果仍未核实。
  - CR-P2-13 的独立 STUN 代码已完成（2ceb37e），不代表已部署。E6 由所有者按 [OPERATIONS §0](OPERATIONS.zh-CN.md) 在维护窗口切换 UDP 7101、外部验证，失败回滚；验证成功后才确认旧 signal 退役。关闭公网 TCP 7100，保留 TCP 7200 与 UDP 7101。
  - 本次文档准备未访问、部署或改动生产服务器。
- **客户端交付与直连边界**：历史 Windows GUI/CLI 包为 U1 之前的版本，Android debug APK 的既有装机证据为模拟器；J1 新产物的走查状态见上。Linux GUI 未构建，macOS/iOS 所有者排除。v1 GUI `ak_task_*` 与 CLI `task` 双入口共享 `task_net`；IPv6 候选、同 socket STUN 与双向打洞已有矩阵模拟证据，但不替代 GUI 视觉或真实跨 NAT 验证；对称↔对称模拟组合按设计干净失败，无中继。

## 最新验证（CI 与本机，分提交）

以下 J0/J4 结果由集成者逐任务取证后提供，原始日志仓外保存；本次文档准备未重新查询 CI。

| 项 | 提交 | CI run | 已取得结论 |
|---|---|---|---|
| J0 合入 | `0a17311` | [43439835214](https://github.com/wmqfl861/AK-sending/actions/runs/43439835214) | 12 success；手动 release-deep-tests skipped |
| J4 钩子语义 | `aee55e0` | [43444609038](https://github.com/wmqfl861/AK-sending/actions/runs/43444609038) | 12 success；手动 release-deep-tests skipped |
| J3 特性隔离 | `8ef4a07` | [43466913061](https://github.com/wmqfl861/AK-sending/actions/runs/43466913061) | 12 success；Android双ABI开关字符串检查success；手动深测skipped |
| J3 netem 特性构建 | `8ef4a07` | [43466913049](https://github.com/wmqfl861/AK-sending/actions/runs/43466913049) | 特权Linux netem任务success，保留模拟定位 |

- 两 run 各自成功的 12 项：Rust Ubuntu/Windows、Flutter、Android、audit、deny、checks、legacy-signal、portability×3、NAT 矩阵。release-deep-tests 的 skipped 不记为执行通过。
- 两 run 原三个新 NAT 案例（late-send-5s、late-send-40s、recv-no-lowttl）均 pass-v4、8 MiB byte-identical；这两次基线证据不计入 J5 固定最终 J2/J3 提交的顺序 10 轮。
- J4 本机隔离验证：clippy、ak-session、ak-cli、send_admission_cli 通过；详细钩子回归范围见审核 §7 CR-P3-17 行。
- **未取得最终证据**：J1 完整走查（Windows 视觉受阻，Android 待跑）、J2/J3 最终提交及完整验收、J5 独立 CI 顺序 10 轮、最终集成提交 CI、C4 真机/Windows TTL 抓包、E6 生产迁移。J6 最终核验待这些对应结果如实回写后进行，不将未执行项补成通过。

以下保留为历史验证，不冒充批次 J 最终结果：

- H/I：run [43421861248](https://github.com/wmqfl861/AK-sending/actions/runs/43421861248)（`97fac96`）常规任务成功，含 NAT 七案、legacy-signal、Flutter、Android、audit、deny、checks、portability×3。
- I1 netem A/B：run [43420288043](https://github.com/wmqfl861/AK-sending/actions/runs/43420288043)（`2dd2507`），RTT≈200 ms、256 MiB：8 MiB 窗口 21.1 MiB/s，quinn 默认 1.25 MiB 窗口 5.6 MiB/s；CR-P2-04 已由 H/I 复核关闭。回环加人工时延，不是广域网结论；可选余量见 CR-P3-19。
- U1 PR #15：run [43433201562](https://github.com/wmqfl861/AK-sending/actions/runs/43433201562)（`ab3d6e5`）常规任务成功；flutter analyze 无问题、app 测试 193/193、ak_ffi_dart 170/170；Rust Ubuntu/Windows、Android（双 ABI、符号校验、APK）、NAT 七案、legacy-signal、audit、deny、checks、portability×3 成功，手动 release-deep-tests skipped。
- 独立复核本机（Linux 容器，rustc stable，无 IPv6）实跑 `97fac96`：全量测试 0 失败，矩阵可运行的纯 IPv4 案例通过，v6 案打印原因后跳过；计数见 H/I 复核第 2 节。
- 本地门槛命令清单见审核文档第 6 节；本页只记结论与适用提交。

## 交接入口

> **当前交接**：[批次 J（进行中，初稿）](HANDOFF-BATCH-J-2026-11-13.zh-CN.md)。
>
> **历史与专项交接**：
> - 第一轮 [HANDOFF-REVIEW-FIXES-2026-11](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md)
> - 批次 F [HANDOFF-BATCH-F-2026-11-12](HANDOFF-BATCH-F-2026-11-12.zh-CN.md)
> - 批次 G [HANDOFF-BATCH-G-2026-11-13](HANDOFF-BATCH-G-2026-11-13.zh-CN.md)
> - 批次 H/I [HANDOFF-BATCH-H-2026-11-13](HANDOFF-BATCH-H-2026-11-13.zh-CN.md)（历史原记录，后续更正见 H/I 复核）
> - 客户端 UI U1 / J1 清单 [HANDOFF-CLIENT-UI-U1-2026-11-13](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
>
> **复核文档**：
> - [验收](REVIEW_2026-11-12-acceptance.zh-CN.md)
> - [批次 F](REVIEW_2026-11-12-batchF.zh-CN.md)
> - [批次 G](REVIEW_2026-11-13-batchG.zh-CN.md)
> - [批次 H/I 与 J 工作单](REVIEW_2026-11-13-batchH.zh-CN.md)

- [HANDOFF](HANDOFF.zh-CN.md)：逐波交接记录、实现与验证边界、重启恢复手册（历史口径，其"当前/进行中"字样以本页为准）。
- [REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md)：审核意见；执行计划（第 5 节）；状态表（第 7 节）；所有者事项及已作决定（第 8、8.1 节）。
- [AGENTS](../AGENTS.md) / [CONTRIBUTING](../CONTRIBUTING.md)：开发规则、红线与门槛。
- [ARCHITECTURE](ARCHITECTURE.zh-CN.md)：模块职责与现状（2026-11-11 口径）。

## 历史去哪查

- [CHANGELOG](../CHANGELOG.md)：用户可感知的里程碑（Keep a Changelog 格式）。
- [BACKLOG](BACKLOG.zh-CN.md)：逐轮任务级完成状态与证据。
- [reports/](../reports/)：逐轮开发验证记录与历次审核报告（wave-28~31、wave-36 终审及其收口复核在内）。
- 各批次的详细过程（含已被更正的口径）见对应交接与复核文档；更早的逐波叙述见 git 历史中的 README/HANDOFF 旧版本。
