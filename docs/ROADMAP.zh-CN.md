# 开发路线与完成定义

## 当前位置（2026-11-05）

按 [ADR-002](adr/002-protocol-stack-and-order.zh-CN.md) D5 顺序，截至第二十四波（2026-11-02 至 2026-11-05，含两轮外部 Codex 评审及其修复波）：

- **P0 大部分完成**：P0-02（无 relay 网络候选收敛＋协调/文件通道隔离证明对）、P0-03（身份认证/配对/密钥保管全链）、P0-04（mDNS 发现＋手动配对＋doctor）已关闭；P0-08 的 ADR 侧已冻结。仍开放：**P0-05/P0-07 需外部网络与物理双机**（受限 NAT/跨运营商样本、真机抓包），P0-01 的"可用真机"部分（现有跨设备证据为 Android 模拟器↔Windows 宿主，矩阵文档见 [SUPPORT_MATRIX](SUPPORT_MATRIX.zh-CN.md)），P0-06 的模拟器/真机走查余项（SAF 代码已落地；iOS 由所有者排除）。
- **P1 全部完成**：P1-01 清单/分页/版本检测、P1-02 有界分块＋摘要＋安全提交、P1-03 双检查点后端＋重启恢复、P1-04 Flutter 双端界面（Windows 桌面＋Android；iOS/macOS 由所有者明确排除）均已勾选关闭。
- **P2 部分完成**：P2-01 代码部分已备（信令＋STUN 协调面、TLS registry 服务器），公网部署另需所有者批准；P2-03 的进程内故障注入形态已备（T07/随机中断套件），端点抓包归 P0-07；P2-02 未开始。
- **邀请码配对已完成**（原 V2-01 规划项提前实现，第十八至二十三波）：`AKI1-` 160 bit 熵邀请码、TLS registry（claim/redeem/revoke/TTL/防枚举）、配对编排 e2e、CLI `invite` 命令族、FFI 19 个 v2 导出、Flutter 邀请页全链落地。
- 已完成（第二十四波 `ac6b23b`）：CLI daemon Fake→Real 接线（RealTransport+SessionRuntime+RealTransferExecutor）。
- 进行中：daemon 真实邀请码并发（终审 P1-02：真实 code_id 分组四模式矩阵与双端 wire 准入，wave-27 专项）；wave-26 修复 Codex 二次终审 20 项发现（`reports/codex-review-2026-11-05-wave25.md`）。

工程形态：13 个 Rust crate＋ak_ffi_dart 纯 Dart 包＋Flutter 桌面/移动应用；测试基线（分波次，见 reports/）：第二十三波 workspace 918/0、第二十四波 955/0（`ac6b23b` 时点）；当前 HEAD 以实际运行为准。Android 模拟器↔Windows 宿主双向真实 QUIC 传输逐字节一致；Windows↔Linux 跨公网双向 8 MiB SHA256 一致（2026-11-05）。验证形态与逐项证据见 [开发验证记录](../reports/dev-validation-2026-11-02.txt) 与 [BACKLOG](BACKLOG.zh-CN.md)；平台支持范围见 [SUPPORT_MATRIX](SUPPORT_MATRIX.zh-CN.md)；里程碑时间线见 [CHANGELOG](../CHANGELOG.md)。本地可执行范围已尽，余项需外部网络、物理双机与部署批准等外部资源。

不将文档齐全或原型代码存在计作 P0 通过；下列工期是 3–4 名全职成员的原始工作量参考，需据实重估，不是自动执行承诺。

| 阶段 | 参考周期 | 交付门槛 |
|---|---|---|
| P0 可行性 | 2–3 周 | 五端构建、Windows/Android/iOS 真机、完全无 relay 公网尝试、LAN 离线、目录授权与后台实验，冻结网络库 |
| P1 局域网可用版 | 4–5 周 | Windows/Android 安全配对、发现、文件夹、保存目录、基本续传、完整性校验 |
| P2 互联网与可靠性 | 4–5 周 | 仅 STUN/信令协调的公网直连、真实失败诊断、重启恢复、路径重验证、无中继证据 |
| P3 五端产品化 | 5–7 周 | macOS/Linux/iOS 完整接入、原件导入、授权持久化、跨端矩阵 |
| P4 发布硬化 | 3–5 周 | 性能、签名升级、安全审查、故障恢复、安装卸载、运维与发布材料 |

顺序工作量约 18–25 周，含缓冲五端稳定版约 20–28 周；单人＋AI 不直接套用该估算。
删除中继部署/费用规划，新增对纯直连成功/失败覆盖、服务边界和隐式回退的验证，不因此承诺更短工期。
P0 就做 iOS，P3 只是补齐完整产品化。真实系统支持矩阵与网络验证结果优先于估算。

## 首个业务里程碑

关闭外网的同一路由网络，Windows↔Android 安全配对、选目录、>4 GiB 文件、断网/恢复、最终摘要一致；同期 iPhone 运行最小收发及授权/后台实验。

## 人员责任

网络/核心一人；Flutter/交互一人；移动与桌面原生适配一人；测试发布由第四人或明确分工兼任。
每阶段都有代码审查、实际测试、已知限制和交接。不得用未运行的测试脚本或占位目录标记完成。
