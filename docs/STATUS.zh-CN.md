# 项目状态

> 更新：2026-11-14（批次 J 复核、main 合并之后）。本页是"当前状态"的**唯一来源**：[README](../README.md) 与 [HANDOFF](HANDOFF.zh-CN.md) 只链接本页。
>
> 写法（CR-P3-20）：本页只写阶段结论、下一步和链接。每项的状态只写在 [审核文档](REVIEW_2026-11-11.zh-CN.md) 第 7 节状态表；run 号与计数只写在当批交接或复核文档的证据表；本页"最新验证"每个 run 只占一行。

## 当前状态

- **阶段**：审核修复第五轮（批次 J）已复核通过，附 2 项 P3（CR-P3-20 记录一致性、CR-P3-21 不稳定测试）。结论见 [REVIEW_2026-11-14-batchJ](REVIEW_2026-11-14-batchJ.zh-CN.md)。
- **已关闭**：
  - CR-P1-07（GUI 打洞时序）；
  - CR-P1-08（矩阵层面；Windows `IP_TTL` 抓包归 C4）；
  - CR-P3-15（保活与 `idle_expired`）；
  - CR-P3-16（调试开关只在 `exp-knobs` 构建里）；
  - CR-P3-17（钩子语义）。
  证据在状态表与批次 J 交接。
- **未完成**：
  - 客户端 UI U1 的走查余项：Windows 深色可读性、Android 系统文件选择器（SAF）普通流程、Android 暗色传输、SafeArea 修复后的首页截图；
  - C4 真机。
  - 项目层面的"通过审核"仍未达到。
- **下一步（批次 K，复核文档第 8 节）**：
  - K0 本地切到 main 并清理分支；K1 走查余项；K2 记录去重与机械校验；K3 修不稳定的探测早退测试；
  - 可选：K4 拆分超大文件、K5 UDP 缓冲；
  - K1 完成后做 C4。
- **分支（2026-11-14 起）**：
  - 仓库只保留 main，main 是已复核的最新状态，**不是发布版**；
  - 每批在从 main 切出的 `dev/batch-<字母>-<日期>` 上工作，推送后 CI 自动运行；
  - 复核通过后由复核方快进合入 main，再由执行方删除该分支；不强推。
  - 2026-11-14 合并时，7 个旧分支（均已包含在 main 中）因复核方会话的 git 代理拒绝删除（HTTP 403）而暂时保留，由本地 agent 在 K0 删除，命令见 [批次 J 复核](REVIEW_2026-11-14-batchJ.zh-CN.md) 第 5 节。
- **待所有者**：
  - E5 签名密钥；E6 服务器部署、防火墙与线上版本核实；E7 LICENSE；
  - CR-P3-03 域名与钉扎轮换；CR-P3-05 SSH 密钥。
  - E1–E4 已决定（见验收文档第 6 节）。
- **生产服务器**：122.51.121.24 的文档记录形态是 registry 7200（TLS）＋旧 signal 7100/7101（2026-11-05），线上版本未核实。
  - CR-P2-13 的独立 STUN 代码已完成（`2ceb37e`），但**未部署**。
  - E6 按 [OPERATIONS §0](OPERATIONS.zh-CN.md) 在维护窗口切换，切换验证完成前不停旧进程，只封 TCP 7100。
  - 本轮未访问、未部署、未改动生产服务器。
- **客户端交付**：
  - Windows GUI/CLI 的已交付包是 U1 之前的版本；Android 只有模拟器装机证据；Linux GUI 未构建；macOS/iOS 所有者排除。
  - 跨 NAT 直连能力（IPv6 候选、同 socket STUN、双向打洞、保活、`idle_expired`）只有 Linux 网络命名空间模拟证据。对称↔对称按设计失败（无中继）。

## 最新验证

- main（`dc8a91b`）：CI run [43558234099](https://github.com/wmqfl861/AK-sending/actions/runs/43558234099) 12 项常规任务全部成功；netem run [43558234106](https://github.com/wmqfl861/AK-sending/actions/runs/43558234106) 成功；NAT 十轮 run [43558234150](https://github.com/wmqfl861/AK-sending/actions/runs/43558234150) 推送时仍在运行，结果由 K0 记入批次 K 交接。
- 开发分支最后一次 CI：run [43544631036](https://github.com/wmqfl861/AK-sending/actions/runs/43544631036)（`4f604bc`），12 项常规任务成功，手动深测按设计跳过。
- NAT 十轮：run [43538159455](https://github.com/wmqfl861/AK-sending/actions/runs/43538159455)（`ad975c0`），10/10 轮、40/40 目标案例 pass-v4（模拟）。
- netem A/B：run [43466913049](https://github.com/wmqfl861/AK-sending/actions/runs/43466913049)（`8ef4a07`），RTT≈200 ms 下 8 MiB 窗口 23.0 MiB/s，默认窗口 5.7 MiB/s（回环加人工时延）。
- 独立复核本机门槛（`4f604bc`）：见 [批次 J 复核](REVIEW_2026-11-14-batchJ.zh-CN.md) 第 2 节。

## 交接入口

> **交接文档**：
> - [批次 J](HANDOFF-BATCH-J-2026-11-13.zh-CN.md)（头部有复核注：部分章节写于十轮重跑之前）
> - [客户端 UI U1 / J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
> - 历史：[第一轮](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md) · [F](HANDOFF-BATCH-F-2026-11-12.zh-CN.md) · [G](HANDOFF-BATCH-G-2026-11-13.zh-CN.md) · [H/I](HANDOFF-BATCH-H-2026-11-13.zh-CN.md)
>
> **复核文档**：
> - [批次 J 与 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md)
> - 历史：[验收](REVIEW_2026-11-12-acceptance.zh-CN.md) · [F](REVIEW_2026-11-12-batchF.zh-CN.md) · [G](REVIEW_2026-11-13-batchG.zh-CN.md) · [H/I](REVIEW_2026-11-13-batchH.zh-CN.md)

- [REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md)：
  - 审核意见；
  - 执行计划（第 5 节）；
  - **状态表（第 7 节）**；
  - 所有者事项（第 8 节）。
- [HANDOFF](HANDOFF.zh-CN.md)：逐波交接记录与重启恢复手册（历史口径）。
- [AGENTS](../AGENTS.md) / [CONTRIBUTING](../CONTRIBUTING.md)：开发规则、红线与门槛。
- [ARCHITECTURE](ARCHITECTURE.zh-CN.md)：模块职责与现状（2026-11-11 口径）。

## 历史去哪查

- [CHANGELOG](../CHANGELOG.md)：用户可感知的里程碑。
- [BACKLOG](BACKLOG.zh-CN.md)：逐轮任务级完成状态与证据。
- [reports/](../reports/)：逐轮开发验证记录与历次审核报告。
- 各批次的详细过程（含已被更正的口径）见对应交接与复核文档。
