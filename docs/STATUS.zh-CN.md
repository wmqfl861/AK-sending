# 项目状态

> 更新：2026-11-14（批次 J 事实同步；最终 run 结果仍待补）。本页是"当前状态"的**唯一来源**：[README](../README.md) 与 [HANDOFF](HANDOFF.zh-CN.md) 只链接本页，不再各自维护状态叙述；历史波次不在此重复。
>
> 逐项修复进度以 [REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md) 第 7 节状态表为准，本页只记口径与结论。

## 当前状态（2026-11-14，批次 J 验收进行中）

- **阶段＝批次 J 实施与验收进行中**。此前批次 H/I 在 `97fac96` 上复核通过，附 1 项 P2、5 项 P3（[REVIEW_2026-11-13-batchH](REVIEW_2026-11-13-batchH.zh-CN.md)）；这是历史批次结论，项目层面的"通过审核"仍未达到。
- **J0、J4 已有对应提交的 CI 证据**：
  - J0：PR #15 已快进合入至 `0a17311`，CI run 43439835214 为 12 success / 1 skipped（手动 release-deep-tests）。
  - J4 / CR-P3-17：`aee55e0` 已推送，隔离树 clippy、ak-session、ak-cli、send_admission_cli 通过；CI run 43444609038 同为 12 success / 1 skipped。钩子按尝试报告结果与 terminal，daemon 仅在终态撤回注册项并打印；两次尝试的回归测试已记录在审核状态表。
  - 两个 run 的结论只适用于各自提交；后续已推送的集成快照为 `534f145`，对应普通CI因NAT超时取消、十轮首跑失败，详见下表。
- **客户端 UI U1 / J1：部分验收，未全部通过**。
  - U1 内容：实时传输进度、正常收尾（CR-P2-16）、连接卡进度、内存最近传输、深色模式。Windows 第 1–5、10 项已取得同机真实 DLL/CLI 行为证据；第 6 项清空及同配置/id-dir 的两个真实 OS 进程重启已补验，两次退出码均 0。首进程注入合成历史，只证明内存生命周期，不冒充新传输。
  - 第 7 项 Windows 光栅纯底色仍阻塞视觉。Android 标准 main 旧基线亮/暗与生码可用；第 8 项亮色辅助真实传输的三行布局/长名截图通过；第 9 项辅助 8 MiB GUI/FFI→Windows 字节数及 SHA256 一致、CLI 退出 0。FilePickGateway 注入 App 沙箱合成源，系统 SAF 普通路径因 Download 权限拒绝仍未验。
  - 200 MiB 辅助传输超测试 4 分钟预算未完成；8 MiB 辅助 runner 整体退出 1（漏采速度行），暗色传输未跑，不能把整套 runner 记为通过。
  - SafeArea `0e1f793`、走查/重启 runner `a9bd53c` 已入库。SafeArea 标准 main 新 APK 构建/安装退出码 0，但复拍首帧期间 emulator 与 services 收到外部 stopped 通知（非执行者发起，来源未知），未重启；Splash 不能作视觉通过证据。自有进程及 17200/17101 无残留，AVD 390dp 覆盖未恢复（原 1080×2400、density420、自动旋转 1 已仓外记录）。
  - 十项明细保留在 [批次 J 交接 §2.1](HANDOFF-BATCH-J-2026-11-13.zh-CN.md)；原清单见 [U1 交接](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)。同机行为、视觉、辅助流程与 C4 真实跨 NAT 分别验收。
- **J2 / CR-P3-15：已完成**。Rust `c60512b`、闲置 UI `6e576d6`、矩阵修复 `593b473` 已入库；最终快照本地全量门禁通过（Rust 1199/0/8、fmt/clippy/rustdoc、契约5/5、Python105/105、Flutter208/208、Dart170/170）。**J5 十轮验收于 `ad975c0` 达成**：run 43538159455 十轮全部通过（10/10 轮、40/40 目标案例 pass-v4），`decline-retry-40s` 每轮实测重试间隔 40000–40001ms、字节一致；普通 CI 43538159415 同时全绿（12 项成功/1 手动跳过）。首轮 534f145 的失败保留为历史时点。
- **J3 / CR-P3-16：已完成于 `8ef4a07`**。精确独立快照完整本地门禁通过；CI run 43466913061 的 12 项常规任务成功、手动深测跳过，Android 双 ABI 发布库无 AK_EXP_ 步骤实际执行成功；netem run 43466913049 成功，Windows 默认 DLL 本地扫描无 AK_EXP_。后续集成版本另按其 run 验证。
- **CR-P1-07、CR-P1-08：代码达标，验收进行中，不关闭**。
  - H/I 复核时三个新案例 6/10 属历史；J0/J4 两 run 的 late-send-5s、late-send-40s、recv-no-lowttl 均 pass-v4、8 MiB byte-identical，不拼接计入本批十轮。
  - **J5 十轮验收达成**：`ad975c0` 的独立 run 43538159455 全部通过（10/10 轮、40/40 目标案例 pass-v4、`decline-retry-40s` 每轮实测 40000–40001ms、字节一致、命名空间清理零残留）。首轮 `534f145` 的 43470613025 失败保留为修复前历史；修复 `593b473` 的进程生命周期回归 11/11 通过。
  - CR-P1-08 的 Windows IPv4 头 `IP_TTL` 抓包仍未取得，由所有者在 C4 执行；单测和 Linux 矩阵不能替代。
- **J6：三者一致性已核**。STATUS 当前状态＋最新验证 → 审核 §7 每行 → 交接 §0/§5 逐行对照完成。J7 / CR-P3-19 本轮不实现。
- **C4 未就绪：仍待 J1、J2、J5 实际完成后，由所有者执行**。J1 尚有视觉与 Android 流程未过，即使 J2/J5 后续成功也不能宣告 ready；Windows TTL 抓包仍属 C4。E6 可并行准备，实际 C4 仍需所有者部署及版本/外部连通验证。
- **E1–E4 已决定且各自工程动作有历史证据**：E1 特性隔离/旧 Dart 层删除（1c95d38、fc53613）；E2 确认窗口 120 s（aa7f3af）；E3 文案（e1ba8b1，UPnP 等仍暂不启用）；E4 删除 MANIFEST.sha256（951b44e）。旧 Rust 代码是否彻底删除按一个发布周期后再评估，不是 E6 后自动删除。
- **待所有者**：E5 签名密钥（正式命名/版本统一与签名验证仍见 CR-P2-02）；E6 服务器部署、防火墙及线上版本核实；E7 LICENSE；CR-P3-03 域名/钉扎轮换；CR-P3-05 SSH 密钥。E5、域名及 SSH 密钥事项不作为新增 C4 前置。
- **生产服务器**：
  - 文档历史记录为 registry 7200（TLS）＋旧 signal 7100/7101（2026-11-05 部署形态）；线上二进制版本和独立 STUN 迁移结果仍未核实。
  - CR-P2-13 的独立 STUN 代码已完成（2ceb37e），不代表已部署。E6 由所有者按 [OPERATIONS §0](OPERATIONS.zh-CN.md) 在维护窗口切换 UDP 7101、外部验证，失败回滚；验证成功后才确认旧 signal 退役。关闭公网 TCP 7100，保留 TCP 7200 与 UDP 7101。
  - 本次文档准备未访问、部署或改动生产服务器。
- **客户端交付与直连边界**：历史 Windows GUI/CLI 包为 U1 之前的版本，Android debug APK 的既有装机证据为模拟器；J1 新产物的走查状态见上。Linux GUI 未构建，macOS/iOS 所有者排除。v1 GUI `ak_task_*` 与 CLI `task` 双入口共享 `task_net`；IPv6 候选、同 socket STUN 与双向打洞已有矩阵模拟证据，但不替代 GUI 视觉或真实跨 NAT 验证；对称↔对称模拟组合按设计干净失败，无中继。

## 最新验证（CI 与本机，分提交）

以下结果由集成者通过GitHub逐任务查询、原始日志及脱敏验收产物核实；`534f145`的失败记录保留，修复后的新提交与结果另列。原始日志仓外保存。

| 项 | 提交 | CI run | 已取得结论 |
|---|---|---|---|
| J0 合入 | `0a17311` | [43439835214](https://github.com/wmqfl861/AK-sending/actions/runs/43439835214) | 12 success；手动 release-deep-tests skipped |
| J4 钩子语义 | `aee55e0` | [43444609038](https://github.com/wmqfl861/AK-sending/actions/runs/43444609038) | 12 success；手动 release-deep-tests skipped |
| J3 特性隔离 | `8ef4a07` | [43466913061](https://github.com/wmqfl861/AK-sending/actions/runs/43466913061) | 12 success；Android双ABI开关字符串检查success；手动深测skipped |
| J3 netem 特性构建 | `8ef4a07` | [43466913049](https://github.com/wmqfl861/AK-sending/actions/runs/43466913049) | 特权Linux netem任务success，保留模拟定位 |
| 修复后集成CI | `ad975c0` | [43538159415](https://github.com/wmqfl861/AK-sending/actions/runs/43538159415) | 12项常规任务success；手动深测skipped |
| **J5十轮验收** | `ad975c0` | [43538159455](https://github.com/wmqfl861/AK-sending/actions/runs/43538159455) | **10/10轮全部通过、40/40目标pass-v4；decline-retry每轮实测≥40s；零残留** |
| 首跑（修复前） | `534f145` | [43470613025](https://github.com/wmqfl861/AK-sending/actions/runs/43470613025) | failure，0/10（历史时点，保留为修复前记录） |
| 集成首跑（修复前） | `534f145` | [43470613069](https://github.com/wmqfl861/AK-sending/actions/runs/43470613069) | cancelled（历史时点） |

- J0/J4 两 run 各自成功的 12 项：Rust Ubuntu/Windows、Flutter、Android、audit、deny、checks、legacy-signal、portability×3、NAT 矩阵。release-deep-tests 的 skipped 不记为执行通过。
- J0/J4 两 run 原三个新 NAT 案例（late-send-5s、late-send-40s、recv-no-lowttl）均 pass-v4、8 MiB byte-identical；这些基线证据不计入 J5 固定 `534f145` 的顺序十轮。
- J4 本机隔离验证：clippy、ak-session、ak-cli、send_admission_cli 通过；详细钩子回归范围见审核 §7 CR-P3-17 行。
- **仍未完成**：J1 Windows视觉、系统SAF普通流程、Android暗色传输及SafeArea修复后标准首页视觉闭环；矩阵脚本修复后的J2/J5十轮与最终集成CI验收；C4真机/WindowsTTL抓包、E6生产迁移。J3自身证据已齐，不再列为WIP，失败及未验结果不补成通过。

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
