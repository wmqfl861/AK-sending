# 项目状态

> 更新：2026-11-13 晚（批次 H/I 复核与客户端 UI U1 之后）。本页是"当前状态"的**唯一来源**：[README](../README.md) 与 [HANDOFF](HANDOFF.zh-CN.md) 只链接本页，不再各自维护状态叙述；历史波次不在此重复。
>
> 逐项修复进度以 [REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md) 第 7 节状态表为准，本页只记口径与结论。

## 当前状态（2026-11-13 晚）

- **阶段＝审核修复第四轮（批次 H、I）已复核：通过，附 1 项 P2、5 项 P3**（[REVIEW_2026-11-13-batchH](REVIEW_2026-11-13-batchH.zh-CN.md)）。
  - CR-P1-07（GUI 打洞时序）与 CR-P1-08（Windows 低 TTL）**代码达标，验收进行中**：矩阵三个新案例 10 连跑 6/10，CR-P1-08 的 Windows 抓包未附。
  - CR-P2-15（事件顺序）通过；CR-P2-04（QUIC 参数）以批次 I 的 A/B 实测关闭。
  - 项目层面的"通过审核"仍待批次 J 与 C4 真机。
- **客户端 UI U1（2026-11-13，Claude 按所有者指示实现，PR #15）**：
  - 内容：实时传输进度、一次配对一次传输的正常收尾（修复 CR-P2-16：发送完成后误报"此连接已断开"）、连接卡进度、"最近传输"、深色模式；契约 ffi-v2 §8.4 U1 修订（只增不改）。
  - 验证：Rust 本地门槛与 PR CI；**无真机或模拟器走查**，留给 J1。
  - 交接：[HANDOFF-CLIENT-UI-U1-2026-11-13](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)。
- **下一步（批次 J，复核文档第 8 节）**：
  - J0 合入 PR #15；J1 U1 走查（Windows 真机 + Android 模拟器）；
  - J2 CR-P3-15（保活在失败重试和闲置超时后失效）；J3 CR-P3-16（调试开关编进正式构建）；J4 CR-P3-17（钩子语义）；
  - J5 矩阵 10 连跑收尾；J6 三者一致收尾流程；
  - 之后做 C4 真机。
- **E1–E4 已由验收方决定**（E1 冻结旧模型；E2 确认窗口 120 s；E3 暂不上 UPnP、对称 NAT 文案已定稿；E4 删除 MANIFEST.sha256）。E5/E6/E7（签名密钥/服务器部署防火墙/LICENSE）待所有者，见验收文档第 6 节。
- **生产服务器**：
  - 122.51.121.24 常驻 registry 7200（TLS）＋旧 signal 7100/7101（2026-11-05 部署形态，线上二进制版本仍未核实）。
  - 本轮未访问、未部署、未改防火墙。
  - **E6 注意**：CR-P2-13（STUN 拆独立服务）完成前不得停旧进程，只能用防火墙封 TCP 7100。
- **客户端交付**：Windows GUI/CLI 已交付打包（U1 之前的版本）；Android debug APK 已构建（模拟器装机，非真机）；Linux GUI 未构建；macOS/iOS 所有者排除。
- **v1 任务码双入口**（GUI `ak_task_*` FFI + CLI `task` 命令族）均已接入。
  - 跨 NAT 直连能力＝接收端 IPv6 候选 + 同 socket STUN 反射 + 双向打洞（配对时 opener、15 秒保活、低 TTL 首轮、反射优先交错拨号；CR-P1-05 至 CR-P1-08，FFI 与 CLI 共用 `task_net`）。
  - 对称↔对称按设计失败（无中继）。
  - 以上为矩阵模拟证据，**不是真机验证**。

## 最新验证（CI）

- Run [43421861248](https://github.com/wmqfl861/AK-sending/actions/runs/43421861248)（批次 I 收官提交 `97fac96`）：全部任务成功，含：
  - NAT 矩阵七案（含 late-send-5s/40s、recv-no-lowttl）、legacy-signal；
  - Flutter、Android（双 ABI、符号校验、APK）；
  - audit、deny、checks、portability×3。
- netem A/B：run [43420288043](https://github.com/wmqfl861/AK-sending/actions/runs/43420288043)（`2dd2507`），RTT≈200 ms、256 MiB：8 MiB 窗口 21.1 MiB/s，quinn 默认 1.25 MiB 窗口 5.6 MiB/s（回环加人工时延，不是广域网结论）。
- PR #15（U1）：run [43433201562](https://github.com/wmqfl861/AK-sending/actions/runs/43433201562)（`ab3d6e5`）全部任务成功：flutter analyze 无问题、app 测试 193/193、`ak_ffi_dart` 170/170；Rust ubuntu/windows、Android（双 ABI、符号校验、APK）、NAT 矩阵七案、legacy-signal、audit、deny、checks、portability×3 全部成功（release-deep-tests 为手动任务，按设计跳过）
- 独立复核本机（Linux 容器，rustc stable，无 IPv6）实跑 `97fac96`：
  - 全量测试 0 失败；
  - 矩阵纯 IPv4 全部可运行案例通过，v6 案按设计打印跳过原因。
  - 计数见复核文档第 2 节。
- 本地门槛命令清单见审核文档第 6 节；本页只记结论，不复述计数。

## 交接入口

> **交接文档**：
> - 第一轮 [HANDOFF-REVIEW-FIXES-2026-11](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md)
> - 批次 F [HANDOFF-BATCH-F-2026-11-12](HANDOFF-BATCH-F-2026-11-12.zh-CN.md)
> - 批次 G [HANDOFF-BATCH-G-2026-11-13](HANDOFF-BATCH-G-2026-11-13.zh-CN.md)
> - 批次 H/I [HANDOFF-BATCH-H-2026-11-13](HANDOFF-BATCH-H-2026-11-13.zh-CN.md)
> - 客户端 UI U1 [HANDOFF-CLIENT-UI-U1-2026-11-13](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
>
> **复核文档**：
> - [验收](REVIEW_2026-11-12-acceptance.zh-CN.md)
> - [批次 F](REVIEW_2026-11-12-batchF.zh-CN.md)
> - [批次 G](REVIEW_2026-11-13-batchG.zh-CN.md)
> - [批次 H/I](REVIEW_2026-11-13-batchH.zh-CN.md)

- [HANDOFF](HANDOFF.zh-CN.md)：逐波交接记录、实现与验证边界、重启恢复手册（历史口径，其"当前/进行中"字样以本页为准）。
- [REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md)：
  - 审核意见；
  - 执行计划（第 5 节）；
  - 状态表（第 7 节）；
  - 待所有者决定清单（第 8 节）。
- [AGENTS](../AGENTS.md) / [CONTRIBUTING](../CONTRIBUTING.md)：开发规则、红线与门槛。
- [ARCHITECTURE](ARCHITECTURE.zh-CN.md)：模块职责与现状（2026-11-11 口径）。

## 历史去哪查

- [CHANGELOG](../CHANGELOG.md)：用户可感知的里程碑（Keep a Changelog 格式）。
- [BACKLOG](BACKLOG.zh-CN.md)：逐轮任务级完成状态与证据。
- [reports/](../reports/)：逐轮开发验证记录与历次审核报告（wave-28~31、wave-36 终审及其收口复核在内）。
- 各批次的详细过程（含已被更正的口径）见对应交接与复核文档；更早的逐波叙述见 git 历史中的 README/HANDOFF 旧版本。
