# 项目状态

> 更新：2026-11-20，批次 S（发布前检查：Android 模拟器真引擎回归、Windows 打包冒烟、全量门槛）执行完毕，待复核。

## 当前状态

批次 S 由本地 agent 在 Windows 上执行完毕，等待复核：

- S0：上一批的分支已清理，main 的 CI 已回填，git branch -a 与 git worktree list 的输出已贴进 S 交接 §8；
- S1：**失败，产品缺陷（模拟器上确认，不是真机验证）**。交叉编译到 x86_64-linux-android 与设备运行正常，传输、整文件摘要校验、时间戳恢复全部完成，接收端在最后“建立最终名”一步报 Permission denied：提交步骤的 hard_link 在 Android 15（API 35，SELinux Enforcing）shell 域的 /data/local/tmp 里被系统策略拒绝，改名与符号链接可用；两次冒烟（默认与 64 MiB）都在同一位置失败，设备内回环复现排除了网络路径。修复方向（硬链接被拒时退回改名）由复核方决定；
- S2：Windows release 打包成功，包内清单与版本提交号已记；用包里的 ak-cli.exe 跑的回环冒烟通过，接收文件逐字节一致；
- S3：全量门槛跑完（结论见 S 交接 §8）。

复核方下一轮实现发送端源文件复核期间的活动信号与接收端磁盘写满的报错，由批次 T 在 Windows 上确认。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

正式完成还取决于所有者事项（E6 部署、C4 物理双机、E5 签名、E7 许可证等），见 [R 复核 §5](REVIEW_2026-11-20-batchR.zh-CN.md#5-余下的工作与批次-s)。

M2 与之后各轮的改动要等 E6 部署后才进入生产服务器。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。

## 最新验证

批次 S 的执行方证据见 [S 交接 §8](HANDOFF-BATCH-S-2026-11-20.zh-CN.md#8-证据表执行方填写)。批次 R 的执行方证据见 [R 交接 §8](HANDOFF-BATCH-R-2026-11-20.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [R 交接 §9](HANDOFF-BATCH-R-2026-11-20.zh-CN.md#9-复核方证据2026-11-20)。批次 Q 的证据见 [Q 交接 §8](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md#8-证据表执行方填写) 与 [§9](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md#9-复核方证据2026-11-20)，批次 P 的见 [P 交接 §8、§9](HANDOFF-BATCH-P-2026-11-19.zh-CN.md#9-复核方证据2026-11-19)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [S 交接（批次 S，当前）](HANDOFF-BATCH-S-2026-11-20.zh-CN.md) · [R 复核（批次 R 验收与批次 S 计划）](REVIEW_2026-11-20-batchR.zh-CN.md) · [R 交接](HANDOFF-BATCH-R-2026-11-20.zh-CN.md) · [Q 复核（批次 Q 验收与批次 R 计划）](REVIEW_2026-11-20-batchQ.zh-CN.md) · [Q 交接](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md) · [P 复核（批次 P 验收与批次 Q 计划）](REVIEW_2026-11-19-batchP.zh-CN.md) · [P 交接](HANDOFF-BATCH-P-2026-11-19.zh-CN.md) · [O 复核（批次 O 验收与批次 P 计划）](REVIEW_2026-11-19-batchO.zh-CN.md) · [O 交接](HANDOFF-BATCH-O-2026-11-18.zh-CN.md) · [N 复核（批次 N 验收与批次 O 计划）](REVIEW_2026-11-18-batchN.zh-CN.md) · [N 交接](HANDOFF-BATCH-N-2026-11-17.zh-CN.md) · [M 记录（批次 M，复核方执行）](HANDOFF-BATCH-M-2026-11-17.zh-CN.md) · [L 复核（验收与批次 M 计划）](REVIEW_2026-11-17-batchL.zh-CN.md) · [L 交接](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 更早：[H/I 复核及 U1 清单](REVIEW_2026-11-13-batchH.zh-CN.md) · [H 交接](HANDOFF-BATCH-H-2026-11-13.zh-CN.md) · [G 复核](REVIEW_2026-11-13-batchG.zh-CN.md) · [G 交接](HANDOFF-BATCH-G-2026-11-13.zh-CN.md) · [F 复核](REVIEW_2026-11-12-batchF.zh-CN.md) · [F 交接](HANDOFF-BATCH-F-2026-11-12.zh-CN.md) · [审核修复轮验收](REVIEW_2026-11-12-acceptance.zh-CN.md) · [审核修复轮交接](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md) · [方案评审（2026-11-02）](REVIEW_2026-11-02.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
