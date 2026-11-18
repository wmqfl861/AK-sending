# 项目状态

> 更新：2026-11-18，批次 O（Windows 确认与 release 深测）。

## 当前状态

批次 O 全部完成并推送。O1 Windows 常规确认通过（workspace 1219/0/8、slow_verify 10/10、ak-storage 57/0）；O2 release 深测全部通过，65,537 文件项首次在 Windows 上通过（2366.2s）。下一步：CI 绿后交复核。生产部署（E6）由所有者执行。

- N0、N1 通过：M2（TLS 帧写入后 flush）在 Windows 上反复运行确认；
- N2 部分通过：M7 在 Windows 上确认，M6 没有逐项记录，65,537 文件项失败；
- N4 没有修好 M-N1：句柄从第一次写入一直开到 finish，而引擎在整个块阶段之后才逐个 finish；CI 回归门槛两次失败后被去掉了 ulimit。

复核方在同一分支上补完并修复，随后快进合入 main：

- M-N1（P1）：文件的最后一块写完即释放 part 文件句柄；CI 在 ulimit -n 256 下跑 2,000 文件测试作为真门槛；Windows 身份查询改用安全封装，ak-storage 恢复 forbid(unsafe_code)；
- N-N1（P1，既有缺陷）：大任务里，接收端准备或校验期间不发帧，发送端会空闲超时。接收端现在边准备边发 ResumeRequest 分片，校验期间发无操作的 Control::Resume，校验阶段检查点改为定时保存；不新增消息类型；
- N-N2（P2）：单线程测试运行时掩盖了 N-N1，t13 测试改在多线程运行时上跑；
- N-N3（P3）：记录与流程问题已更正，包括一次强推。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

下一步：本地 agent 执行 [批次 O](HANDOFF-BATCH-O-2026-11-18.zh-CN.md)：

- O0：清理已合入的分支，回填 main 的 CI；
- O1：Windows 常规测试，确认复核方的改动；
- O2：Windows release 深测，逐项记录，65,537 文件项必须通过；
- O3（可选）：CR-P2-08 生产代码拆分。

M2 与本轮的改动要等 E6 部署后才进入生产服务器。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。

## 最新验证

批次 N 的执行方证据见 [N 交接 §8](HANDOFF-BATCH-N-2026-11-17.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [N 交接 §9](HANDOFF-BATCH-N-2026-11-17.zh-CN.md#9-复核方证据2026-11-18)；批次 O 的证据由执行方记在 [O 交接 §8](HANDOFF-BATCH-O-2026-11-18.zh-CN.md#8-证据表执行方填写)。批次 M 的证据见 [M 记录 §3](HANDOFF-BATCH-M-2026-11-17.zh-CN.md#3-证据表)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [O 交接（批次 O 起点，当前）](HANDOFF-BATCH-O-2026-11-18.zh-CN.md) · [N 复核（批次 N 验收与批次 O 计划）](REVIEW_2026-11-18-batchN.zh-CN.md) · [N 交接](HANDOFF-BATCH-N-2026-11-17.zh-CN.md) · [M 记录（批次 M，复核方执行）](HANDOFF-BATCH-M-2026-11-17.zh-CN.md) · [L 复核（验收与批次 M 计划）](REVIEW_2026-11-17-batchL.zh-CN.md) · [L 交接](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
