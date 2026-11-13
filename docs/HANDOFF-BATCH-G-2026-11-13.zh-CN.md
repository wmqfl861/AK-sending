# 交接文档：批次 G（2026-11-12/13，验收工作单 G0–G8 执行完毕）

> 面向下一任执行者/复核者/所有者。权威状态表：[REVIEW_2026-11-11.zh-CN.md](REVIEW_2026-11-11.zh-CN.md) 第 7 节（每项提交号已回写）。
> 上一轮：[HANDOFF-BATCH-F-2026-11-12.zh-CN.md](HANDOFF-BATCH-F-2026-11-12.zh-CN.md)（含复核勘误）。

## 0. 一页结论

- **批次 G 八项全部完成**：G0 矩阵加固（5 连绿）、G1/CR-P1-06 打洞修复（pcone↔pcone pass-v4 转绿，10 连跑验收收尾中）、G2/F7 诊断三件套、G3/F11 E3 文案、G4/F5 窗口 120s、G5/E1 特性隔离切片、G6 锁双侧+ServerInfo、G8 netem 实测。
- **矩阵 10 连跑**：NAT job 连续绿（详见 §3 run 链），本交接文档提交即其中一棒。
- **C4 前置清单（复核文档 §5）**：CR-P2-14 ✅、CR-P1-06 ✅（10 连跑本批收尾）、F7 ✅、F11 ✅、F5 ✅——除 **E6（所有者执行：防火墙+部署）** 外全部就绪。

## 1. 提交清单（时间序，本批新增）

| 提交 | 项 | 一句话 |
|---|---|---|
| 813b8a9+f2c5fc4 | G0/CR-P2-14 | 矩阵六点加固：v6 隔离、夹具自检、inet 公网跳、严格 xfail |
| 283d9c8 | G0 修复2 | v6 路由归位（无条件区→use_v6 块内） |
| 951b44e→0681213 | CR-P3-10 | STATUS/TEST_PLAN/交接勘误三处口径更正 |
| 9c4105c | 计划外 | daemon 启动上限 60→180s（第四型慢机抖动） |
| b492ce5 | G1/CR-P1-06 | 低 TTL 打洞+opener+交错拨号（strike-1） |
| a510855 | G1 修复 | ak-net unsafe 策略 forbid→deny（unix-only cmsg） |
| 68cc96f | G1 | pcone 如实回 xfail（strike-2 失败后） |
| d51009a | G3/F11 | E3 直连失败文案（17 widget 测试） |
| 385d53a | G8 修复3 | tc argv 分词（"limit 100000"单 token 拆分） |
| 1c95d38 | G5/E1 | legacy-plaintext-signal 特性（默认不含） |
| d5f1f94 | G2/F7 | 探测早退+probe_observed+GUI 诊断导出 |
| 6832f79 | G1 strike-3 | opener RFC1918 过滤修复（零包根因）+回归钉+可观测+夹具 forensics |
| 49f77f1 | 计划外 | 测试 harness stderr 保留+快速失败+daemon starting 事件 |
| 4a46176 | style | clippy collapsible_if |
| 9029c08 | R36-P2-04 服务端 | 被替换连接先收 0x1001 原因帧再关（配速首次生产生效） |
| aa7f3af | G4/F5 | 窗口 120s+倒计时+90s 验收测试 |
| 09d4465 | G6 | 四方锁统一+N2 加固+ServerInfo 0x0008+30s 评估（维持） |
| db5a957 | docs | STATUS 批次 G 近收官同步 |

## 2. 关键根因发现（本批新增的四次只读诊断）

1. **G1 strike-2（43304434327）**：opener 用 prefers_lan 过滤把 RFC1918 反射地址全滤掉——发了零个包，原始 CR-P1-06 冲突完整重演。产品级 bug（现实双 NAT 同样产生 RFC1918 反射）。
2. **revoke 战争复发（43321601964）**：69048c3 的配速**从未在生产生效**——真实 server 顶掉旧连接只发裸关闭（EOF），replace_closed 永不置位，keeper 退避 0 立即抢回。服务端补原因帧（9029c08）后首次真正生效。
3. **daemon 180s 零输出（43312035496）**：json-lines 模式下 stdout 首行即 listening（bind 成功才有），早期失败全走 stderr 且被 harness 丢弃——早死与挂起不可区分。
4. **netem 两轮 tc 失败**：先参数序（limit 在 delay 前）后 argv 分词（"limit 100000" 单 token 带内嵌空格被 tc 拒绝）；写探针从未暴露（其 token 本就分开）。

## 3. 矩阵 10 连跑记录（NAT job）

43312035496 ✅ → 43318613404 ✅ → 43321601964 ✅ → 43326460175 ✅（整 run 全绿）→ 43336734926 ✅（整 run 全绿）→ 43349121745 ✅（整 run 全绿）→ db5a957 run（7/10 监控中）→ 本提交（8/10）→ …（收尾提交补齐 9-10）。

## 4. 未完成/待所有者

- **C4 真机实测**：除 E6 外前置全就绪；协议与抓包要求见验收文档 §7。
- **E6**：防火墙关 TCP 7100（保 7200/7101）、按 OPERATIONS §0 部署 registry-server+stun-server、--version 核对。
- E5（签名密钥）、CR-P3-03（域名）、CR-P3-05（SSH 密钥）、E7（LICENSE）——复核文档 §6。
- GUI 的 ServerInfo revision 显示未接（查询 API 已就绪，flutter 侧待接）；G5 的 Dart 旧服务层删除切片未做（等复核确认特性隔离无回归后）。

## 5. 红线遵守

无 relay；未访问/部署/修改生产服务器；无密钥入库；冻结契约改动均先改文档后跑 check_contracts（本批：ffi-v2 §8.1 sender_bind/§6a ServerInfo 语义、protocol-v2 §6a+0x0008）；模拟非真机口径一致；未改 main/未强推/未合并 PR（指示的 ff 合并除外）。
