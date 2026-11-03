# 实施清单

所有未勾选项目均未完成。领域骨架不等于网络实现。

## 初始化

- [x] 将纯直连决策固化为 ADR-001。
- [x] 整理需求、架构、路线、协议和验收文档。
- [x] 创建 Rust 策略/分块/任务状态模型与测试源码。
- [x] Rust 工具链实编译、测试和 clippy（2026-11-02，rustc 1.97.0；修复 1 处 clippy 报错）；证据见 reports/rust-validation-2026-11-02.txt。
- [x] 取得目标仓库可写权限并验证远端提交；证据见 reports/GITHUB_PUBLISH_VERIFICATION.zh-CN.md。
- [x] 方案评审；见 docs/REVIEW_2026-11-02.zh-CN.md。
- [x] 所有者指示"继续本地开发"后按评审建议路线推进，固化为 ADR-002（2026-11-02，本地分支）；D2/D3 的产品文案与端口映射默认值仍待所有者确认。

## 局域网原型（ADR-002 / 评审第五节第 1 步，2026-11-02/03，分支 dev/local-parallel）

- [x] 模块化实现：ak-identity/ak-wire/ak-storage/ak-net/ak-discovery/ak-store/ak-signal/ak-engine/ak-cli/ak-itests 十一 crate（两轮并行子代理开发）；workspace 281 项测试、clippy `-D warnings` 全绿；证据见 reports/dev-validation-2026-11-02.txt。
- [x] 真实身份认证最小实现（P0-03 部分）：Ed25519 设备密钥、指纹、配对码、自签证书钉扎（含常量时间比较与严格 DER 校验）；错误钉扎/未授权路径发送前拒绝（T06 自动化，engine 测试）。
- [x] QUIC 直连数据面（P0-02 部分）：quinn + rustls/ring，双向自签证书验证，`DirectChannel` 无公开构造（D6 类型约束）。
- [x] 文件清单/目录分页扫描/源版本检测（P1-01）：LocalSource 扫描 + BLAKE3 全量/分块摘要 + mtime 版本 + SOURCE_CHANGED 检测；迭代式深目录扫描（200 层实测）。
- [x] 有界分块读写、摘要验证、临时文件安全提交（P1-02）：路径越界/符号链接防护（Windows 实测）、.akpart 原子落盘、透明续传、接收端 mtime 保真（尽力而为）、磁盘余量查询、孤儿 part 清理工具。
- [x] 检查点与重启恢复（P1-03 的文件后端）：原子写任务记录；传输中断→检查点/半文件存活→二次运行续传（T04/T10 自动化，engine 测试）。
- [x] >4 GiB 大文件（T12 自动化形态）：5 GiB 稀疏文件 release 环回通过（u64 偏移、内存有界、整体摘要一致），回归 96 秒。
- [x] 海量文件（T13 缩样）：2,000 文件通过；**20,000 文件 release 通过（328s）**；过程中发现并修复 ResumeRequest 4,096 条目上限缺陷（场景测试发现→引擎代理修复→4,097 边界实测转完整路径）。单 job 上限现为 65,535 条目（u16 字段，分页为协议演进项）。
- [x] 引擎增强：空闲超时（30s 默认，IdleTimeout 错误、检查点保留）、Pause/Resume 协议（对端暂停→发送端挂起，无任何回退）、可选多流并行（能力协商，每文件一 QUIC 流，默认 opt-in，单流回退有测试钉死）、字节级进度事件。
- [x] CLI 产品化：配对簿（pair add/list/remove、簿内准入、未配对拒绝、TOFU 记录）、任务管理（jobs list/clean，不动 .akpart）、接收循环（Ctrl-C 退出、断点可续传）、发现集成（配对状态列、--json）、防火墙提示、--multistream 开关。
- [x] 场景测试套件（ak-itests）：T02/T05/T06/T09/T10/T11/T13 的环回 L2 形态映射（11 项测试 + 20k release 变体）；TEST_PLAN 文档含逐项映射表。
- [x] 信令+STUN 协调面（P2-01 代码部分，未部署）：封闭消息集测试锁定、限频、会话 TTL；仓库检查脚本扫描 Cargo.lock 禁 relay/TURN/WebRTC/iroh 依赖（D6）。
- [x] 公网直连探测工具（评审第 2 步的本地形态）：probe 命令（注册候选、STUN 映射、UDP 同时打开、同 socket 钉扎 QUIC 握手、按 device id 字典序定角色）；跨网段实测仍属 P0-05，未执行。
- [x] mDNS 发现（P0-04 部分）：本机组播环回实测可用；AP 隔离/防火墙场景未测。
- [x] 局域网 CLI 双端传输：真实双进程环回传输逐字节一致（含空目录、嵌套、二进制大文件、多流模式、配对簿准入）。
- [x] 方案评审第 6 节代码建议：job.rs 增加等待接收方确认状态；生产路径身份由类型保证。
- [x] 文档对照：TEST_PLAN 自动化映射、ARCHITECTURE 模块现状、SECURITY 已实现机制与已知限制、TECH_SELECTION 状态更新。

第三轮（2026-11-03，提交 `5ac6f2a`，4 个并行子代理＋集成者修复；证据见 reports/dev-validation-2026-11-02.txt"第三轮并行开发验证"节）：

- [x] 存储满预检（T09 相关）：`ak-engine::check_headroom` 纯决策函数（余量未知→放行、恰好放行、不足→`EngineError::StorageFull` 携真实数字）；接收端在应答 ResumeRequest 前实测目的目录余量并经协议 `ErrorCode::StorageFull` 告知对端；耗尽仍由真实写错误兜底，不伪造成功。
- [x] 多根合并发送：`SendSpec::extra_roots`/`SourceSet` 将多个文件/目录合并为一个清单（每根条目加 `<根名>/` 前缀），根名重复在建立连接前拒绝；无额外根时单根行为字节级不变；`Report.roots` 记录各根名；CLI `send` 直接接受多路径。
- [x] 同出口提示（评审 4.1）：会话两端在协调面的 TCP 对端 IP 一致时，服务器在 SessionOpened 后向双方各发一条 `PeerSignal::SameEgress`（仅候选排序提示，非信任判定；IP 未知不发提示；设备重连后按新 IP 刷新）；封闭消息集测试同步更新（ak-signal）。
- [x] T07 场景环回自动化：真实进程内信令服务器完成完整会话时序后关停（传输前/传输中两变体；传输中变体断言关停后仍有新 chunk 事件），直连传输照常完成、字节一致（ak-itests t07_coordination_down.rs）。
- [x] T03 场景 IPv6 环回形态自动化：`classify(::1)`/`fe80::/10` 判定、双端绑 `[::1]:0` 的 QUIC+TLS 握手互钉、引擎双绑定 `[::1]:0` 传输字节一致且接收端实际绑定地址为 ::1（ak-itests t03_ipv6_loopback.rs）；跨网段/公网 IPv6 直连仍属 T03 未覆盖部分。
- [x] T04 数据面零信令依赖证明对：先证明死端口上协调面确实不存在（连接拒绝），随后在从未启动任何信令服务的机器上引擎传输照常完成（ak-itests t04_signal_irrelevant_to_data.rs）；与 T07 构成证明对——协调面可有可无，从未处于文件数据路径（ADR-001 不变量）。
- [x] 环回吞吐基准（信息性，非验收门槛）：release、64×8 MiB＝512 MiB 确定性载荷，单流 97.9 MiB/s、多流 103.3 MiB/s（仅 1.06x，环回收益有限；真实网卡吞吐未测）；`ak-cli bench`（进程内环回、--bytes/--files/--chunk_kb、固定种子可复现）＋ ak-itests bench_throughput.rs（只断言正确性，不断言速度下限）。
- [x] connect 族缺陷修复（场景测试发现→集成者修复→回归钉死）：IPv4 socket 拨 IPv6 对端原本会无限悬挂于 QUIC 重试（IPv6 场景测试发现）→ `connect_bounded` 地址族不匹配快速失败（InvalidInput）＋连接期空闲预算包裹，回归测试 `connect_family_mismatch_fails_fast_instead_of_hanging` 钉死。
- [x] CLI 第三轮：`--timeout` 空闲预算透传（send/receive，默认 30s，非总运行时长）、单行刷新进度渲染（`\r` 刷新＋79 列裁剪，AK_QUIET 静默）；第三轮后 workspace 308 项测试通过（第二轮后为 281 项）、clippy `-D warnings` 全绿、5 GiB release 回归 84.3s。

第四轮（2026-11-03，7 个并行子代理；证据见 reports/dev-validation-2026-11-02.txt"第四轮并行开发验证"节）：

- [x] ResumeRequest 多帧分页（协议演进）：每帧携带 more 标志，接收端分片发送、发送端逐帧累积并校验 job_id；单任务条目数不再受单帧 65,535 上限约束；100,000 条目分片单元验证（2 片 65,535+34,465 全量按序到达），65,537 文件 release 边界实测（t13 ignored 变体）；PROTOCOL 文档已对齐。
- [x] SQLite 检查点后端（P1-03 正式项）：`SqliteCheckpointStore`（可选特性 sqlite，rusqlite bundled，默认依赖图零泄漏）；事务原子性、双连接 WAL 并发、与文件后端语义等价（ak-store 默认 10 + 特性 23 测试）。
- [x] 连接统计 API（无中继运行时旁证）：`DirectChannel::stats()` 暴露 tx/rx 字节与帧数、丢包计数；真实环回对账测试（帧数跨端一致、字节差 ≤10%）；`accept_within` 超时辅助。
- [x] T06 深化（身份替换攻击面）：mid-transfer 端口被他人密钥抢占→钉扎拒绝且零落盘；正确指纹对第三方证书→握手期拒绝；配对簿准入端到端（簿内传输/簿外拒绝）（ak-itests t06_identity_swap.rs）。
- [x] T08 可行形态：端口变更→旧地址有界拒绝→同身份新地址重钉扎→同 job_id 续传完成（ak-itests t08_reconnect_recheck.rs；QUIC 连接迁移不在范围，已注明）。
- [x] T10 多流扩展：多流模式中断→part/checkpoint 存活→二次多流续传一致；多流+多根组合一次通过（ak-itests t10_multistream_resume.rs）。
- [x] CLI 第四轮：多路径发送（send p1 p2 dir）、指纹 8–63 位唯一前缀解析（查配对簿）、probe SameEgress 提示（一次性）；真实双进程冒烟（3 根多流 + 前缀端到端）通过。
- [x] 发现模块第四轮：候选携带设备名（TXT 清洗：控制字符替换、UTF-8 边界截断 64B）、browse_with 实时回调（addr+fp 去重）、watch 持续发现模式；本机真实组播回环验证。
- [x] 第四轮后 workspace 338 项测试通过（含 sqlite 特性 +23）、clippy 双配置全绿。

第五轮（2026-11-03，4 个并行子代理＋集成者接线；证据见 reports/dev-validation-2026-11-02.txt"第五轮并行开发验证"节）：

- [x] 策略模型接入真实通道（架构闭环）：`authorize_path` 闸门在 send/receive 拿到 DirectChannel 后、任何协议帧之前执行——LocalDirect 恒过、InternetDirect 按 `DirectPolicy::allow_internet`、Relay/Unknown 防御性拒绝（`EngineError::RouteForbidden`，连接以专用码关闭）；Observation 布尔的等价物由类型保证（DirectChannel=已认证直连），不重复填写；CLI `send/receive --no-internet` 旗标接线，LAN-only 环回冒烟通过。
- [x] `ak-cli doctor` 环境自检：身份/配对簿、本地网络（v4/v6 分列）、双栈 UDP 绑定、mDNS 尽力探测（本机回环 ok）、可选 STUN（--stun）、磁盘余量；每项 ok/warn/skip＋结论建议（防火墙弹窗/AP 隔离指引）；--json。
- [x] 发送端友好错误映射：14 个错误码全量中文建议（StorageFull 双数值、IdleTimeout 续传指引、RouteForbidden 策略说明等），anyhow context 保留完整错误链。
- [x] 块尺寸扫描基准（信息性）：256MiB×{64KiB,256KiB,1MiB,4MiB}×{单流,多流} 实测——64KiB 显著最差（-23%），256KiB–1MiB 平台期 ~99 MiB/s，4MiB+多流本轮 108 MiB/s（单轮无中位数，结论仅 64KiB 劣势可靠；真实网卡最优值待双机数据）。
- [x] 文档现状化：README 快速上手全旗标核对、UX"CLI 已实现交互"现状节（配对三形态/接收循环/进度渲染，与原文分歧以现状注记对冲）、OPERATIONS 协调服务器运维节（限额默认、无文件路由三层验证、SameEgress 误报场景、日志边界）。
- [x] 第五轮后 workspace 364 项测试通过、clippy 全绿、5 GiB release 回归 111.49s。

第六/七轮（2026-11-03，3+3 个并行子代理＋集成者；证据见 reports/dev-validation-2026-11-02.txt"第六/七轮并行开发验证"节）：

- [x] 并发接收：`ak_engine::serve_concurrently`（一次 bind、每连接独立任务/检查点、失败任务不拖垮他者、accept 非取消安全处理）；CLI `receive --jobs N`；真实双发送端冒烟 2/2 字节一致。
- [x] 双栈端点（评审 4.1 IPv6 优先深化）：`bind_dual_stack`（V6ONLY=0）、v4-mapped classify 展开、`local_addr_display` 候选 API、`holepunch::bind_dual_stack_udp`、CLI `probe --dual-stack`；本机实测可用（含 socket2 `only_v6` Windows 坑与间歇 WSAENETUNREACH 如实记录）。
- [x] 基准刷新（空闲机）：20k 文件 263.7s（较旧值快 20%）；512MiB 单流 93.9 / 多流 97.9 MiB/s（1.04x）；块尺寸扫描见第五轮。
- [x] 长文件名 part 缺陷修复（场景测试发现→集成者修复）：249 字节文件名 × `.akpart` 后缀超 NTFS 255 上限 → `.akpart-<hash16>` 短名回退（确定性、续传兼容）；边界测试翻转为完成路径；孤儿清理识别两种 part 形态。
- [x] T11 极端名边界：248 字节满长名/CJK/emoji/单字符/近 260 与 `\?\` 超长路径往返一致；尾随点/空格 18 形态校验层拒绝。
- [x] probe --json（15 字段稳定形状，双进程实测）；pair export/import（AKPAIR1 文本块，私钥哨兵验证不泄漏，坏行容错合并）；--version 携带 wire v1/caps/profile。
- [x] wire 帧类型诊断名 + ak-wire 直接依赖去镜像；检查点 file→sqlite 迁移工具（3 好 1 坏→3/1 语义）。
- [x] 信令对抗加固：500 帧 TCP 模糊、STUN 畸形包洪泛、限频旁路全部存活；发现并修复 Hello 设备名控制字符注入（codec 层拒绝 C0/DEL/C1），ak-wire 显示字符串同规则修复（测试锁定）。
- [x] 六/七轮后 workspace 404 项测试（＋sqlite 特性 26）全绿、clippy 干净。

第八轮（2026-11-03，3 个并行子代理＋集成者；证据见 reports/dev-validation-2026-11-02.txt"第八轮并行开发验证"节）：

- [x] 产品全流程 e2e（ak-itests e2e_product_flow.rs 四幕）：配对簿准入+未配对第三方零字节拒绝；16MiB/256 块中断→part 存活→同 job_id 续传→检查点 state==completed 且全量已验证前缀；serve_concurrently(2) 多根+单根并发（Report.roots 单/多根两种落盘语义如实断言）；信令会话建立→服务器死亡证明→直传照常。首个业务里程碑的单机可执行子集闭环。
- [x] CLI：completions（bash/zsh/fish/powershell）、jobs status <id>（每文件 verified 前缀视图）、receive --bind 与 signal-server --bind/--stun-bind（IPv6 监听实测 [::1]）。
- [x] 文档：SECURITY（并发隔离论证/双层注入防护/对抗加固/长名 part 回退，+7 条）、ARCHITECTURE（模块现状 5 行更新+接口 2 行，FileTransferEngine 过期行修正）。
- [x] 第八轮后 workspace 417 项测试全绿、clippy 干净。

## P0

- [ ] P0-01 固定系统/ABI/发行版支持矩阵及可用真机。
- [ ] P0-02 比较无 relay 网络候选，验证控制协调与文件通道隔离。
- [ ] P0-03 真实身份认证、配对和密钥保管最小实现。
- [ ] P0-04 局域网发现、离线手动配对、权限拒绝恢复。
- [ ] P0-05 IPv4/IPv6、受限 NAT、禁 UDP、跨运营商网络样本。
- [ ] P0-06 Android URI/iOS 保存目标读写、原件导入、后台中断。
- [ ] P0-07 真机传输抓包与库默认中继审计。
- [ ] P0-08 输出网络库 ADR、风险记录及重估路线。

## P1–P2

- [ ] P1-01 实现文件清单/目录分页扫描/源版本检测。
- [ ] P1-02 有界分块读写、摘要验证、临时文件安全提交。
- [ ] P1-03 SQLite checkpoint 和重启恢复。
- [ ] P1-04 Flutter 双端界面、设置、重名处理与错误说明。
- [ ] P2-01 部署控制面实验环境（另获部署批准）；无文件接口。
- [ ] P2-02 路径切换先停正文再验证；移动数据授权。
- [ ] P2-03 协调故障/直连故障注入及端点抓包。

## P3–P4

- [ ] P3-01 完成五端原生适配与 20 个有向组合。
- [ ] P3-02 大文件、海量文件、长时间恢复、目录撤权。
- [ ] P4-01 安装签名、升级兼容、许可证选择、发布材料。
- [ ] P4-02 外部安全审查和正式无中继证据报告。

## 后续

- [ ] V2-01 一次性邀请与撤销授权。
- [ ] V2-02 联系人、可选账号和多设备管理。

禁止加入“中继兜底”“云端暂存”作为任何未完成任务的临时绕行实现。
