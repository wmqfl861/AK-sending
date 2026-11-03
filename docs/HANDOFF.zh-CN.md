# 当前交接

## 目标与授权

仓库：`wmqfl861/AK-sending`；默认分支：`main`；本地开发分支：`dev/local-parallel`（未推送）。
用户已授权上传本项目全部文档和代码，并完成目标账户连接。
服务器仅可用于设备发现、信令和 NAT 穿透协调，绝不承载文件数据或密文；ADR-001 不变。
2026-11-02（第二阶段）：用户指示"继续本地开发，多模块多子代理并行，直到项目完成，不用推送"；本地分支推进，未推送、未部署、未强推、未加许可证。

## 第二至八轮完成内容（2026-11-02 至 2026-11-03，dev/local-parallel）

按 ADR-002（采纳评审 D1/D5/D6 路线）完成局域网 CLI 原型全栈：

- 提交历史：`c13ecb8`（脚手架/接口冻结）→ `925c63d`（六模块并行实现）→ `4a3bf36`（引擎+CLI 集成与验证）→ `911bc31`（第二轮：引擎增强/配对 CLI/存储补强/场景测试）→ `5ac6f2a`（第三轮：四并行代理＋集成者修复）→ `0140282`（第四、五轮：十并行代理＋接线）→ `9961506`（65,537 边界补记）→ `ab33a46`（第六、七轮：六并行代理＋集成者修复）→ `d1fa222`（六/七轮 BACKLOG 记录）→ `3b39c03`（第八轮：产品流程 e2e/CLI 打磨/文档对齐）→ `cbdb2a4`（第八轮 BACKLOG 记录）。
- 11 个 crate：ak-core（领域）、ak-identity、ak-wire、ak-storage、ak-net、ak-discovery、ak-store、ak-signal、ak-engine、ak-cli、ak-itests（跨 crate 场景测试）。
- 数据面：QUIC（quinn 0.11）+ rustls/ring + 自签 Ed25519 证书双向钉扎；`DirectChannel` 无公开构造函数（类型约束）；无 relay/TURN 路径，Cargo.lock 依赖扫描锁定。
- 引擎：完整端到端协议（见 ak-engine 文档），确定性任务 ID、窗口化确认、检查点续传、完成标记晚于校验+落盘；第三轮追加存储满预检（`check_headroom` 决策函数＋STORAGE_FULL 协议路径）与多根合并发送（`extra_roots`/`SourceSet`，`Report.roots`）。
- 协调面：信令 TCP + STUN UDP 单进程（`ak-cli signal-server`），封闭消息集、限频、会话 TTL，无文件路由（测试锁定）；第三轮追加同出口提示（`PeerSignal::SameEgress`，两端 TCP 对端 IP 一致时各发一条，仅排序提示非信任判定）。
- 验证证据：[开发验证记录](../reports/dev-validation-2026-11-02.txt)——第八轮后 workspace 417 项测试（＋sqlite 特性 26）、clippy `-D warnings`、仓库检查、12 项 Python 测试、5 GiB 稀疏 release 环回、真实双进程 CLI 传输逐字节一致、65,537 文件分页边界 release 实测完成（257 页清单）、产品全流程 e2e 四幕（ak-itests）。

第四/五轮新增（2026-11-03，提交 `0140282`，11 个并行子代理＋集成者）：ResumeRequest 多帧分页（单任务条目上限解除，100k 单元验证＋65,537 边界实测）；SQLite 检查点后端（可选特性 `sqlite`，默认依赖图零泄漏，事务原子性）；`DirectChannel::stats()` 连接统计（无中继运行时旁证）；T06 身份替换攻击面三场景、T08 端口变更重评估、T10 多流续传（ak-itests）；CLI 多路径发送＋指纹唯一前缀解析＋SameEgress 提示；发现模块设备名/browse_with/watch；DirectPolicy 策略闸门接入真实通道（`--no-internet`，零字节进入协议）；`ak-cli doctor` 环境自检；发送端 14 错误码友好映射；块尺寸扫描基准（64KiB 最差 -23%）；README/UX/OPERATIONS 现状化。

第三轮新增（2026-11-03，提交 `5ac6f2a`，4 个并行子代理＋集成者）：

- ak-engine：存储满预检、多根合并发送（根名重复连接前拒绝）、`Report.roots`；connect 族缺陷修复——IPv4 socket 拨 IPv6 对端原会无限悬挂于 QUIC 重试（IPv6 场景测试发现）→ `connect_bounded` 地址族快速失败＋连接期空闲预算（集成者修复）→ 回归测试 `connect_family_mismatch_fails_fast_instead_of_hanging` 钉死（场景测试发现→集成者修复→回归钉死）。
- ak-signal：`PeerSignal::SameEgress` 同出口提示（评审 4.1），IP 未知不发、重连刷新记录，封闭消息集测试同步更新。
- ak-itests：T07 协调面关停环回形态（传输前/传输中两变体，关停后仍有新 chunk 事件）、T03 IPv6 环回（引擎双绑定 `[::1]:0` 字节一致）、T04 数据面零信令依赖证明对（死端口拒绝＋无信令服务传输照常）；吞吐基准（release、512 MiB、环回）：单流 97.9 MiB/s、多流 103.3 MiB/s。
- ak-cli：`bench` 命令（进程内环回证据生成器，--bytes/--files/--chunk_kb、固定种子）、`--timeout` 空闲预算透传（send/receive）、单行刷新进度渲染；`send` 接受多路径合并发送。

第六/七轮新增（2026-11-03，提交 `ab33a46`，3+3 个并行子代理＋集成者修复）：并发接收 `ak_engine::serve_concurrently`（一次 bind、每连接独立任务/检查点、失败任务不拖垮他者；CLI `receive --jobs N`，真实双发送端冒烟 2/2 字节一致）；双栈端点（`bind_dual_stack` V6ONLY=0、v4-mapped classify 展开、`local_addr_display`、`holepunch::bind_dual_stack_udp`、CLI `probe --dual-stack`，本机实测可用并如实记录 socket2 only_v6 Windows 坑）；空闲机基准刷新（20k 文件 263.7s 较旧值快 20%；512MiB 单流 93.9/多流 97.9 MiB/s）；长文件名 part 缺陷修复（T11 场景发现 249 字节文件名 × `.akpart` 超 NTFS 255 上限 → `.akpart-<hash16>` 确定性短名回退，续传兼容，边界测试翻转为完成路径）＋ T11 极端名边界（满长名/CJK/emoji/`\\?\` 超长路径往返一致、尾随点/空格 18 形态校验层拒绝）；`probe --json`（15 字段稳定形状，双进程实测）、`pair export`/`import`（AKPAIR1 文本块，私钥哨兵验证不泄漏）、`--version` 携带 wire v1/caps/profile；wire 帧类型诊断名与依赖去镜像、检查点 file→sqlite 迁移工具（3 好 1 坏→3/1 语义）；信令对抗加固（500 帧 TCP 模糊、STUN 畸形包洪泛、限频旁路全部存活；发现并修复 Hello 设备名控制字符注入，codec 层拒绝 C0/DEL/C1，ak-wire 显示字符串同规则修复并测试锁定）。六/七轮后 workspace 404 项测试（＋sqlite 特性 26）全绿、clippy 干净。

第八轮新增（2026-11-03，提交 `3b39c03`，3 个并行子代理＋集成者）：产品全流程 e2e 四幕（ak-itests `e2e_product_flow.rs`——配对簿准入＋未配对第三方零字节落盘前拒绝；16MiB/256 块中断→part 存活→同 job_id 续传→检查点 completed 且全量已验证前缀；`serve_concurrently(2)` 多根+单根并发；信令会话建立→服务器死亡证明→直传照常）；CLI 打磨（`completions` bash/zsh/fish/powershell、`jobs status <id>` 每文件 verified 前缀视图、`receive --bind` 与 `signal-server --bind`/`--stun-bind`，IPv6 监听实测 `[::1]`）；文档对齐（SECURITY 并发隔离论证/双层注入防护/对抗加固/长名 part 回退，ARCHITECTURE 模块现状与 FileTransferEngine 过期行修正）。第八轮后 workspace 417 项测试全绿、clippy 干净。

## 实现与验证边界（如实）

- 已验证范围＝本机环回（L2 形态）：QUIC 握手、传输、续传、大文件、CLI 双进程。跨机器、跨网段、公网 NAT 穿透、抓包（L3/L4）未执行。
- 打洞：holepunch 探测与 probe 命令已实现并有本地 UDP 互探测试；真实 NAT 环境成功率未测（P0-05）。
- 移动端/Flutter、真机验收、性能调优（多流并发、千兆跑满）未开始。
- 已知限制见各 crate 文档与 SECURITY：接收端 available_bytes 为真实实现（fs2）、mtime 尽力回写（filetime）、IPv6 前缀按 /64 简化、probe nonce 非密码学随机（身份由 QUIC 握手承担）、SameEgress 存在反代/CGNAT 误报场景（仅排序提示）。

## 历史记录（第一阶段，main 分支）

README 初始化提交：`4923b178860de198867f6de7b7d9cb7942200bb3`。
完整原包提交：`5ce974c279acac2bb27bd24ba4e703042dde2861`。
2026-11-02 方案评审见 [方案评审](REVIEW_2026-11-02.zh-CN.md)；D1/D5/D6 已按所有者指示经 ADR-002 采纳，D2/D3 细节仍待确认。
第一阶段验证记录见 [Rust 验证记录](../reports/rust-validation-2026-11-02.txt)、[上传验收记录](../reports/GITHUB_PUBLISH_VERIFICATION.zh-CN.md)。

## 下一工程节点

1. 两台真实机器（同网段）跑 `ak-cli receive`/`send`，验证非环回路径与防火墙行为；随后 `signal-server` + `probe` 在家宽/热点组合下实测公网直连成功率（P0-05，决定产品文案）。多流并行在环回仅约 1.04–1.06x（第六轮空闲机刷新：单流 93.9 vs 多流 97.9 MiB/s），是否默认启用待双机网卡数据再定。
2. 所有者确认 D2（公网失败提示文案）与 D3（端口映射默认开关）。
3. 视实测结果冻结网络库 ADR（评审 D1 尾项）并重估路线。
4. Flutter UI（Windows → Android 优先）与移动端后台/存储适配。
经第八轮后复核，上述节点仍然成立且顺序不变；本地可执行范围已尽，余项均需外部资源（第二台机器/真实网络/Flutter SDK/移动工具链）。
后续开发先读取本文件、AGENTS、ROADMAP 和现有源码；保留已有历史，禁止强推。
