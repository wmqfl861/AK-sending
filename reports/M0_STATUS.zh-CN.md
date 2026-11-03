# M0 总验收状态

截至2026-11-02（America/Los_Angeles），受测且独立复审工程基线 `253cc6c601253170e6847f59aef3c8bbd0c03d5f`。
**总状态：PARTIAL_PHYSICAL_DEVICE_GATES_NOT_RUN。独立LAN原生驱动与五端操作入口的代码缺口已经补齐，但实机退出验收没有完成。**

## 模块与证据

| 模块 | 工程状态 | 证据 |
|---|---|---|
| M0-01 授权领域与工程基线 | REVIEWED | [审核](M0-01_REVIEW.zh-CN.md) |
| M0-02 认证直连适配器 | REVIEWED | [审核](M0-02_REVIEW.zh-CN.md) |
| M0-03 有界会话与授权流 | REVIEWED | [审核](M0-03_REVIEW.zh-CN.md) |
| M0-04 原生链接与FFI | REVIEWED_NATIVE_PROBE_ONLY | [审核](M0-04_REVIEW.zh-CN.md) |
| M0-05 Flutter/移动风险入口 | 工程REVIEWED，实机PARTIAL | [审核](M0-05_REVIEW.zh-CN.md) |
| M0-06 独立两端LAN驱动 | REVIEWED_LAN_HARNESS_ONLY | [审核](M0-06_REVIEW.zh-CN.md) |
| M0-07 五端LAN页及证据工具 | 工程REVIEWED，实机PARTIAL | [审核](M0-07_REVIEW.zh-CN.md)、[机器索引](M0-07_VALIDATION.json) |

各计划先于实现独立提交，审核绑定具体SHA；历史证据不覆盖。PR #7/#8及前序PR仍未合并main，没有部署、采购、强推或生产签名。

## 退出条件与未完成项

| 条件 | 当前证据 | 结论 |
|---|---|---|
| 固定工具链/锁文件/只读CI | Rust/Flutter/SDK输入固定，最终源归档与Git树一致 | PASS_工程，runner不是位级固定 |
| 真实认证与本机授权/失败关闭 | 110项核心Rust、真实UDP/TLS/负例 | PASS_已测范围，不证明全部网络 |
| 两端独立运行 | 三桌面独立进程；Linux两个无默认路由网络栈 | PASS_实验，不是物理Wi-Fi |
| 五端LAN操作入口 | 地址/临时证书/完整摘要核对/本机确认/取消，五端release | PASS_代码与构建，不是五端实机 |
| 手机权限/目录/生命周期 | 平台代码编译、主机Swift回归、Dart组件负例 | PARTIAL，系统实际行为NOT_RUN |
| 两台真实设备、无公网Wi-Fi | 新入口与步骤已就绪，未接入真实设备 | BLOCKED_DEVICE_EVIDENCE |
| 最低OS、权限撤销、目录重启复用、后台/锁屏/杀进程 | 原始设备证据缺失 | BLOCKED_DEVICE_EVIDENCE |
| 轻量预算 | 当前极小实验产物字节已记录，无手机CPU/内存/启动数据 | PARTIAL，不宣称完整产品达标 |

最终CI：core41568269666、native41568269744、Flutter41568269677、mobile41568269667均成功。三桌面每系统110核心Rust、16native Rust；Linux/macOS61 Python，Windows58通过+3跳过；每Flutter主机任务20 Dart；Apple主机6 Swift。所有数字按用例而非重复执行累加。

## 下一执行节点：实机验收，不重复开发已有入口

[Issue #9](https://github.com/wmqfl861/AK-sending/issues/9)集中跟踪；按[双设备操作手册](../experiments/flutter_probe/LAN_DEVICE_RUNBOOK.zh-CN.md)和[设备矩阵](../experiments/flutter_probe/DEVICE_MATRIX.zh-CN.md)执行。模板所有未测值继续NOT_RUN/null。单设备校验器只能证明记录结构与哈希一致，不证明物理真实性或整体M0批准。

必须实际取得两端安装、WAN断开/蜂窝关闭但Wi-Fi保留、系统权限正负例、目录/进程生命周期、最低系统与资源数据。当前环境未接入物理设备，不能填写PASS。实际发现缺陷后按模块规则先计划、修复、复测和审核，再更新总验收。

当前只传固定合成数据，没有用户文件流式I/O、BLAKE3完整性、可靠落盘、持久续传或公网NAT产品。它们仍分别归M1/M2，不将本次M0实验宣称为可交付文件客户端。无后台继续工作的承诺；交接者先读远端最新HEAD与AGENTS。
