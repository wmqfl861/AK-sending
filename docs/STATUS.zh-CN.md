# 项目状态

> 更新：2026-11-11。本页是"当前状态"的**唯一来源**：[README](../README.md) 与 [HANDOFF](HANDOFF.zh-CN.md) 只链接本页，不再各自维护状态叙述；历史波次不在此重复。
>
> 逐项修复进度以 [REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md) 第 7 节状态表为准，本页只记口径与结论。

## 当前状态（2026-11-12）

- **阶段＝审核修复轮已完成并通过独立验收复核（有条件）**。第一轮修复（`dev/review-fixes-20261111`，32 个提交，审核文档 [REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md) 全部 19 项编号处置完毕）由 Claude 独立复核：**通过（条件＝本轮 F0 文档收尾，见 [REVIEW_2026-11-12-acceptance](REVIEW_2026-11-12-acceptance.zh-CN.md)）**；新发现 8 项（P1×1/P2×4/P3×3）已补入审核文档第 4.5/7 节。
- **当前执行（2026-11-12 晚）**：批次 F 执行至 nat-matrix 三修停止点。F0/F9/F8/F3/F4/F1/F2 已完成（CR-P1-05 双向打洞落地，NAT 矩阵 **3/4 通过**，含 v4 反射地址端到端证据）；唯 symm↔symm 意外成功=对称 NAT 模拟保真度问题，三选项待所有者（见 [HANDOFF-BATCH-F-2026-11-12](HANDOFF-BATCH-F-2026-11-12.zh-CN.md) §2）。F5/F7/F10/F11/F6 未开始、无技术阻塞。nat-matrix job 在所有者表态前持续红（已知状态，非回归）；其余 11 job 全绿。
- **复核更正（2026-11-12 晚，Claude，见 [REVIEW_2026-11-12-batchF](REVIEW_2026-11-12-batchF.zh-CN.md)）**：symm 案的定性不成立——是矩阵的 IPv4 NAT 案没有隔离 IPv6，产品走了 IPv6 直连，不是模拟保真度问题（纯 IPv4 复跑时 symm↔symm 按预期失败）。纯 IPv4 下 **pcone↔pcone 失败**（conntrack 元组冲突改写了发送端外部端口），新编号 **CR-P1-06（P1）**；CR-P1-05 改判为"结构达标、验收未达成"；矩阵修复记为 CR-P2-14。下一棒为批次 G（先 G0 修矩阵，再 G1 修 CR-P1-06）。
- **E1-E4 已由验收方决定**（E1 冻结旧模型：删旧 Dart 层随时做、旧 Rust 特性隔离须在 F3 之后；E2 确认窗口 30s→120s；E3 暂不上 UPnP、对称 NAT 文案已定稿；E4 删除 MANIFEST.sha256）。E5/E6/E7（签名密钥/服务器部署防火墙/LICENSE）待所有者，见验收文档第 6 节。
- **生产服务器**：122.51.121.24 常驻 registry 7200（TLS）＋旧 signal 7100/7101（2026-11-05 部署形态，线上二进制版本仍未核实）。本轮未访问、未部署、未改防火墙。**E6 注意：CR-P2-13（STUN 拆独立服务）完成前不得停旧进程，只能防火墙封 TCP 7100。**
- **客户端交付**：Windows GUI/CLI 已交付打包；Android debug APK 已构建（模拟器装机，非真机）；Linux GUI 未构建；macOS/iOS 所有者排除。
- **v1 任务码双入口**（GUI `ak_task_*` FFI + CLI `task` 命令族）均已接入；跨 NAT 直连能力＝接收端 IPv6 候选 + 同 socket STUN 反射 + 单向探测（完整双向打洞见 CR-P1-05/F2）。

## 最新验证（CI）

- Run [43121806432](https://github.com/wmqfl861/AK-sending/actions/runs/43121806432)（修复轮收官提交 `ae0b4e7`）：全绿——Rust ubuntu/windows、flutter（186+166）、android（双 ABI+符号校验+APK 上传）、audit、deny、checks、portability×3；release-deep-tests 手动任务按设计 skipped。
- 独立验收复核另在 Linux 容器（rustc 1.97.0、无 IPv6、网卡 192.0.2.2/24）实跑全量门槛：1,203 通过 / 0 失败 / 7 忽略（验收文档第 2 节）。
- 本地门槛命令清单见审核文档第 6 节；本页只记结论，不复述计数。

## 交接入口

> **交接文档**：第一轮修复 [HANDOFF-REVIEW-FIXES-2026-11](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md)；批次 F [HANDOFF-BATCH-F-2026-11-12](HANDOFF-BATCH-F-2026-11-12.zh-CN.md)（含 nat-matrix 三轮记录与待决三选项）。

- [HANDOFF](HANDOFF.zh-CN.md)：逐波交接记录、实现与验证边界、重启恢复手册（历史口径，其"当前/进行中"字样以本页为准）。
- [REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md)：本轮审核意见＋执行计划（第 5 节）＋状态表（第 7 节）＋待所有者决定清单（第 8 节）。
- [AGENTS](../AGENTS.md) / [CONTRIBUTING](../CONTRIBUTING.md)：开发规则、红线与门槛。
- [ARCHITECTURE](ARCHITECTURE.zh-CN.md)：模块职责与现状（2026-11-11 口径）。

## 历史去哪查

- [CHANGELOG](../CHANGELOG.md)：用户可感知的里程碑（Keep a Changelog 格式）。
- [BACKLOG](BACKLOG.zh-CN.md)：逐轮任务级完成状态与证据。
- [reports/](../reports/)：逐轮开发验证记录与历次审核报告（wave-28~31、wave-36 终审及其收口复核在内）。
- 更早的逐波叙述见 git 历史中的 README/HANDOFF 旧版本。
