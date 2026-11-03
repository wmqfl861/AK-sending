# M0 设备验收矩阵

当前状态：**BLOCKED_DEVICE_EVIDENCE_NOT_RUN**。本轮未接入可操作的物理Android/iOS及两台真实Wi-Fi设备；CI构建、主机回归或隔离网络栈不改变下列格子。

M0-06独立LAN native/CLI与M0-07五端LAN操作页面已实现，并在受测代码253cc6c601253170e6847f59aef3c8bbd0c03d5f构建/审核通过。旧“LAN驱动未实现”缺口已关闭，但系统实际行为仍需下面的实验。[操作手册](LAN_DEVICE_RUNBOOK.zh-CN.md) · [证据模板](DEVICE_EVIDENCE.template.json) · [跟踪Issue #9](https://github.com/wmqfl861/AK-sending/issues/9)。

| 项目 | Android最低/当前目标 | iOS最低/当前目标 | 必要原始证据 |
|---|---|---|---|
| 安装与冷启动 | NOT_RUN | NOT_RUN | 源码SHA、设备/OS、实际安装产物摘要、10次启动时间 |
| LAN权限允许/拒绝/撤销 | NOT_RUN | NOT_RUN | 系统动作与短状态；真实LAN，非loopback |
| 目录选择/取消/读写/删除 | NOT_RUN | NOT_RUN | 本机测试目录、自有64字节新文件校验与清理 |
| 重启后持久授权、撤权失败关闭 | NOT_RUN | NOT_RUN | 冷启动复用/拒绝，不记录目录位置 |
| 云提供者离线/超时/清理失败 | NOT_RUN | NOT_RUN | 提供者类别、短状态、任务和清理边界 |
| 前后台/锁屏/系统杀进程 | NOT_RUN | NOT_RUN | 取消/release、恢复不自动启动、临时文件检查 |
| 双设备无公网Wi-Fi | NOT_RUN | NOT_RUN | 使用新LAN页面，WAN断开/蜂窝关闭但Wi-Fi保留 |
| 空闲CPU/内存/体积预算 | NOT_RUN | NOT_RUN | release产物、正确内存指标、10分钟空闲采样 |

目标Android API26与37、iOS15与26；核验实际硬件与目标兼容性，不把CI主机版本当最低系统。桌面Windows11/Ubuntu24.04/macOS13最低目标也须明确物理/虚拟机运行证据。macOS当前构建仅arm64。

退出准则：必要项目均有精确源码/构建归属的真实记录并经审核，才可修改M0总状态。未测保持NOT_RUN/null，失败保留FAIL和复现。记录检查器只验证结构/路径/哈希一致，不证明设备真实性；不能凭退出0或人工填写PASS自动批准整个M0。不上传个人路径、地址、私钥或配对证书。
