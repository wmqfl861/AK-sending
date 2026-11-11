# 项目状态

> 更新：2026-11-11。本页是"当前状态"的**唯一来源**：[README](../README.md) 与 [HANDOFF](HANDOFF.zh-CN.md) 只链接本页，不再各自维护状态叙述；历史波次不在此重复。
>
> 逐项修复进度以 [REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md) 第 7 节状态表为准，本页只记口径与结论。

## 当前状态（2026-11-11）

- **阶段＝审核修复阶段**。2026-11-11 全项目独立审核（[REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md)，审核对象 `main@88c77e0`，发现 P1×5/P2×9/P3×5＋2 项 wave-36 遗留）；修复在分支 `dev/review-fixes-20261111` 上按批次 **A 恢复门禁 → B 安全与可用性 → C 跨 NAT 直连 → D 移动端功耗与生命周期 → E 瘦身与文档** 推进。A 与 B 已完成（个别收尾项依赖所有者决定，见审核第 8 节），CI 全绿；**批次 C（跨 NAT 直连，CR-P1-02）进行中**；D1（自适应轮询＋生命周期观察者）已落地，D2/D3 与批次 E 待做。
- **生产服务器**：122.51.121.24 常驻 registry 7200（TLS）＋旧 signal 7100/7101（2026-11-05 部署形态）。线上二进制版本未核实（审核 CR-P2-09）；本轮修复未访问、未部署、未改防火墙（部署与防火墙归所有者）。
- **客户端交付**：Windows GUI（v1 极简 UI）与 CLI（`ak-cli`，v1 任务码 `task` 命令族＋旧邀请码两种模式）已交付并打包（`scripts/package_windows.ps1`）；Android debug APK 已构建并在模拟器装机（**非真机**，v1 UI 走查未执行）；Linux GUI 从未在 Linux 主机构建；macOS/iOS 由所有者明确排除。平台逐项状态见 [SUPPORT_MATRIX](SUPPORT_MATRIX.zh-CN.md)。
- **v1 一次性任务码模型双入口**：GUI（`ak_task_*` FFI→Dart→Flutter）与 CLI（`ak-cli task`，不依赖 ak-ffi）均已接入（wave-28/wave-32）。
- **已知最大缺口**：v1 主线尚不能跨 NAT 直连——候选地址只有本机 IPv4/回环，无 STUN 反射候选、无 IPv6，两端均处 NAT 后即无可拨地址（批次 C 处理中）；两台物理机（家宽/热点）NAT 穿透成功率与 L3 抓包级无中继证据未做。

## 最新验证（CI）

- Run [43011364868](https://github.com/wmqfl861/AK-sending/actions/runs/43011364868)（批次 A 收口）：全绿——Rust ubuntu/windows、audit、portability×3、checks；release-deep-tests 为手动任务按设计 skipped。
- Run [43031622309](https://github.com/wmqfl861/AK-sending/actions/runs/43031622309)（批次 B 时点）。
- 本地门槛命令清单见审核文档第 6 节；本页只记 CI 结论，不复述本地测试计数。

## 交接入口

> **本轮审核修复的完整交接**：[HANDOFF-REVIEW-FIXES-2026-11](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md)（33 提交、CI 证据链、逐项验证方法、待决清单）。

- [HANDOFF](HANDOFF.zh-CN.md)：逐波交接记录、实现与验证边界、重启恢复手册（历史口径，其"当前/进行中"字样以本页为准）。
- [REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md)：本轮审核意见＋执行计划（第 5 节）＋状态表（第 7 节）＋待所有者决定清单（第 8 节）。
- [AGENTS](../AGENTS.md) / [CONTRIBUTING](../CONTRIBUTING.md)：开发规则、红线与门槛。
- [ARCHITECTURE](ARCHITECTURE.zh-CN.md)：模块职责与现状（2026-11-11 口径）。

## 历史去哪查

- [CHANGELOG](../CHANGELOG.md)：用户可感知的里程碑（Keep a Changelog 格式）。
- [BACKLOG](BACKLOG.zh-CN.md)：逐轮任务级完成状态与证据。
- [reports/](../reports/)：逐轮开发验证记录与历次审核报告（wave-28~31、wave-36 终审及其收口复核在内）。
- 更早的逐波叙述见 git 历史中的 README/HANDOFF 旧版本。
