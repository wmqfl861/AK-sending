# 开发总结（面向项目所有者）

生成于 2026-11-03（第九轮，`dev/local-parallel` 分支，未推送）。本文所有数字与结论逐一来自 [开发验证记录](archive/dev-validation-2026-11-02.txt)、[BACKLOG](../docs/BACKLOG.zh-CN.md) 与 [HANDOFF](../docs/HANDOFF.zh-CN.md)，2026-11-03 本轮对关键计数做了复测；本文不引入未经记录的新事实。2026-11-04 补记：第十至十六轮简表与最新状态一览见文末"第 10-16 轮补章"（Rust 计数于 2026-11-04 本机实测，Dart/Flutter 取集成轮报告值并标注统计口径；正文历史段落保持 2026-11-03 原貌）。

## 目标（ADR-002 路线）

- 约束不变（ADR-001）：服务器仅用于设备发现、信令与 NAT 穿透协调，绝不承载文件数据或密文；无 relay/TURN/云端暂存回退（Cargo.lock 依赖扫描由仓库检查脚本锁定）。
- 路线（[ADR-002](../docs/adr/002-protocol-stack-and-order.zh-CN.md)，采纳方案评审 D1/D5/D6）：数据面 = QUIC（quinn）+ rustls（ring）+ 自签 Ed25519 证书 + 指纹钉扎；控制面 = 自建信令（TCP，封闭消息集）+ 内置最小 STUN；推进顺序 = 先局域网 Rust 命令行原型，再公网直连探测工具，Flutter 五端界面后置；直连约束以类型/依赖/测试机制落地（`DirectChannel` 无公开构造函数、信令消息集封闭且测试锁定、T04 行为测试）。
- 授权背景：2026-11-02 项目所有者指示"继续本地开发，多模块多子代理并行，直到项目完成，不用推送"。

## 九轮并行开发历程

每轮为若干并行子代理＋集成者；"轮末测试"为该轮结束时 `cargo test --workspace` 通过数。仓库共 20 个提交：前 5 个（`4923b17`→`eed9fc9`）为初始化、蓝图导入与方案评审，其后 15 个为九轮开发提交。

| 轮 | 日期 | 并行子代理 | 主题 | 关键交付 | 轮末测试 | 提交 |
|---|---|---|---|---|---|---|
| 一 | 11-02 | 6＋集成者 | 基础模块并行实现与集成 | 接口契约冻结后六模块并行实现；引擎＋CLI 集成与端到端验证；probe 同 socket 打洞→钉扎 QUIC 握手（真实双进程实测）；ak-itests 脚手架 | 242（另 1 ignored） | `c13ecb8`、`925c63d`、`4a3bf36`、`b488091`、`511f78e`、`0599ed0` |
| 二 | 11-02 | 5 | 引擎增强／配对 CLI／存储保真／场景测试／文档映射 | 空闲超时＋Pause/Resume＋多流 opt-in；配对簿准入、jobs、接收循环；T02/T05/T06/T09/T10/T11/T13 环回映射与 TEST_PLAN 映射表；修复 ResumeRequest 4,096 条目上限缺陷 | 281 | `911bc31` |
| 三 | 11-03 | 4＋集成者 | 存储预检／同出口提示／T07＋IPv6＋T04／bench | `check_headroom` 存储满预检、多根合并发送、`PeerSignal::SameEgress`、T07/T03/T04 场景自动化、`bench` 与 `--timeout`；修复 connect 挂起缺陷 | 308 | `5ac6f2a` |
| 四 | 11-03 | 7 | 分页／SQLite／连接统计／T06/T08/T10／CLI 多路径／发现增强 | ResumeRequest 多帧分页（100,000 条目单元验证）、SQLite 检查点后端（默认依赖图零泄漏）、`DirectChannel::stats()`、T06 三场景/T08 重钉扎/T10 多流续传、设备名＋browse_with/watch | 338（另 sqlite 特性 23） | `0140282`（四/五轮合并提交） |
| 五 | 11-03 | 4＋集成者 | 策略闸门／doctor／错误映射／块基准／文档现状化 | `authorize_path` 策略闸门接入真实通道（`--no-internet`）、`doctor` 环境自检、14 错误码全量中文映射、块尺寸扫描基准、README/UX/OPERATIONS 现状化；65,537 文件分页边界 release 实测补记 | 364 | `9961506`、`7bf90e6`（代码见 `0140282`） |
| 六 | 11-03 | 3＋集成者 | 并发接收／双栈端点／基准刷新／T11 极端名 | `serve_concurrently` 并发接收＋`receive --jobs`、`bind_dual_stack` 双栈、空闲机基准刷新（20k 文件 263.7s）；发现并修复 249 字节文件名 part 缺陷 | —（与第七轮合并计） | `ab33a46` |
| 七 | 11-03 | 3＋集成者 | probe --json／配对导出导入／迁移工具／信令对抗 | `probe --json`（双进程实测）、`pair export/import`（私钥哨兵验证不泄漏）、检查点 file→sqlite 迁移；信令对抗加固（TCP 模糊/STUN 洪泛/限频旁路全存活）并修复 Hello 设备名控制字符注入（ak-wire 显示字符串同规则） | 404（另 sqlite 特性 26） | `ab33a46`、`d1fa222` |
| 八 | 11-03 | 3＋集成者 | 产品全流程 e2e／CLI 打磨／文档对齐 | e2e 四幕（配对准入/中断续传/并发接收/信令死亡证明）、`completions` 四 shell、`jobs status`、`--bind` 系列（IPv6 监听实测 `[::1]`）、SECURITY/ARCHITECTURE 对齐 | 417 | `3b39c03`、`cbdb2a4` |
| 九 | 11-03 | 文档代理 | 总结报告与计划文档刷新 | 本文＋[TEST_PLAN](../docs/TEST_PLAN.zh-CN.md)/[ROADMAP](../docs/ROADMAP.zh-CN.md) 刷新；417＋26 计数于本轮复测确认 | 417（不变） | 不提交（按指示） |

## 最终形态

### 11 个 crate

| crate | 职责与要点 |
|---|---|
| ak-core | 直连策略、分块、任务状态领域模型；零第三方依赖（仓库检查锁定） |
| ak-identity | Ed25519 设备密钥、BLAKE3 指纹、配对码、PeerBook、自签证书钉扎（常量时间比较、严格 DER 校验） |
| ak-wire | 数据面帧协议；封闭帧类型集（诊断名）、显示字符串控制字符防护 |
| ak-storage | 清单扫描＋BLAKE3 全量/分块摘要、有界分块读写、`.akpart` 原子提交与长名 `<hash16>` 回退、路径越界/符号链接防护、mtime 尽力保真 |
| ak-net | `DirectEndpoint`/`DirectChannel`（无公开构造，仅校验路径产生）、路径分类、`connect_bounded`（地址族快速失败＋空闲预算）、同 socket 打洞、双栈绑定、`stats()` 连接统计 |
| ak-discovery | mDNS 发现（本机实测可用）、候选设备名 TXT、本地优先/IPv6 优先排序、browse_with/watch |
| ak-store | 检查点双后端：行式文件＋可选 SQLite（rusqlite bundled，默认依赖图零泄漏）、file→sqlite 迁移工具 |
| ak-signal | TCP 信令服务器/客户端＋内置 STUN（RFC 5389 子集）；封闭消息集、限频、会话 TTL、对抗加固 |
| ak-engine | 端到端引擎：确定性 job id（BLAKE3）、窗口化确认、检查点续传、多流（能力协商 opt-in）、`serve_concurrently` 并发接收、存储满预检、策略闸门 |
| ak-cli | 全部能力的产品面（命令清单见下） |
| ak-itests | 跨 crate 场景测试：T02–T13 环回形态、吞吐/块尺寸双基准、产品全流程 e2e 四幕 |

### CLI 能力清单（ak-cli）

- 身份与配对：`keygen`、`fingerprint`；`pair add/list/remove/code/export/import`（AKPAIR1 文本块，导出含私钥哨兵验证）
- 发送：`send`（多路径合并为一个 job、`--to`、`--fp` 支持 64 hex 或配对簿 8–63 位唯一前缀、`--multistream`、`--no-internet`、`--timeout`、`--conflict error|rename`）
- 接收：`receive`（默认循环服务至 Ctrl-C、`--once`、`--jobs N` 并发、`--bind` v4/v6、`--expect` 配对簿/any/指纹/配对码、`--no-internet`、`--timeout`）
- 发现与任务：`discover`（mDNS 有界浏览＋配对状态列、`--json`）；`jobs list/status/clean`（不动 `.akpart`）
- 运维与探测：`doctor`（身份/网络 v4v6/双栈 UDP/mDNS/可选 STUN/磁盘，ok|warn|skip＋建议、`--json`）；`signal-server`（信令 TCP 默认 7100＋STUN UDP 默认 7101，`--bind`/`--stun-bind`，IPv6 监听实测）；`probe`（注册/候选交换/STUN 映射/UDP 同时打开/同 socket 钉扎 QUIC 握手，`--dual-stack`/`--json`）
- 工具：`bench`（进程内环回基准，`--bytes/--files/--chunk-kb/--multistream/--once/--json`）；`completions`（bash/zsh/fish/powershell）；`--version` 携带 wire v1/caps/profile

### 协议要点

- 控制面封闭消息集：无 payload/blob/文件路径/清单/摘要字段，未知字段拒绝；认证、会话、候选数量与消息频率独立限额；服务器无任何文件路由（测试锁定）。
- 数据面：QUIC＋自签 Ed25519 证书双向钉扎；Hello/Offer/Accept/ManifestPage/Chunk/ChunkAck/ResumeRequest/Pause/Finalize/FinalAck/Error；文件大小/偏移 u64 语义，先验证再分配内存。
- 完整性：逐块 BLAKE3 摘要＋整文件摘要；源漂移报 SOURCE_CHANGED；完成标记严格晚于校验＋落盘。
- 恢复：确定性 job id；ResumeRequest 自第四轮起多帧分页（more 标志，单任务条目数不再受单帧 65,535 约束）；检查点"临时文件→持久化→记录→确认"顺序。
- 策略与运维：`authorize_path` 闸门（LocalDirect 恒过、InternetDirect 按 `--no-internet`、Relay/Unknown 防御拒绝）；STORAGE_FULL 携真实数字；SameEgress 同出口提示（仅排序，非信任判定）；codec 层拒绝 C0/DEL/C1 控制字符。

## 质量证据

### 测试与静态检查（2026-11-03 本轮复测确认）

- `cargo test --workspace`：417 项通过、0 失败、5 项 ignored（全绿）。
- `cargo test -p ak-store --features sqlite`：26 项通过。
- `cargo clippy --workspace --all-targets -- -D warnings`（含 sqlite 特性双配置）：零警告。
- `python scripts/check_repository.py`（L0）：通过——workspace 成员、ak-core 零依赖、Cargo.lock 禁 relay/TURN/WebRTC 扫描、本地链接检查。
- `python -m unittest discover -s scripts/tests`：12 项通过。

### release 深测清单（默认 ignored 的 5 项，均执行通过）

| 项目 | 结果 |
|---|---|
| 5 GiB 稀疏文件（ak-engine，u64 偏移） | 首次 73.68s；历次回归 95.96s→84.3s→111.49s |
| 20,000 文件（ak-itests t13 release 变体） | 首轮 328s；第六轮空闲机刷新 263.7s（快 20%） |
| 65,537 文件分页边界（t13 ignored 变体） | files=65537 bytes=65537、pages=257、send=2339.9s recv=2340.0s wall=2410.1s（与其他构建任务并行争用 CPU，结果 test ok） |
| 吞吐基准 bench_throughput（512 MiB，release） | 第三轮：单流 97.9 MiB/s、多流 103.3 MiB/s（1.06x）；第六轮空闲机：93.9 / 97.9 MiB/s（1.04x）——环回收益有限，真实网卡未测 |
| 块尺寸扫描 bench_chunk_sweep（256 MiB×8） | 64KiB 显著最差（-23%）；256KiB–1MiB 平台期 ~99 MiB/s；4MiB＋多流本轮 108 MiB/s（单轮数据，仅 64KiB 劣势结论可靠） |

### 四次缺陷闭环（发现者→修复者→钉死测试）

| 缺陷 | 发现者 | 修复者 | 钉死测试 |
|---|---|---|---|
| ResumeRequest 单帧 4,096 条目上限（20k 场景下中断） | 第二轮海量文件场景测试 | 第二轮引擎子代理 | `ak-itests/tests/t13_many_files.rs` 4,097 条目边界（实测转完整路径） |
| connect 阶段无超时：IPv4 socket 拨 IPv6 对端无限悬挂 | 第三轮 IPv6 场景测试（t03） | 集成者（`connect_bounded` 地址族快速失败＋空闲预算） | `ak-engine connect_family_mismatch_fails_fast_instead_of_hanging` |
| 249 字节文件名 × `.akpart` 后缀超 NTFS 255 上限 | 第六轮 T11 极端名场景测试 | 集成者（`.akpart-<hash16>` 确定性短名回退，续传兼容） | `ak-itests/tests/t11_extreme_names.rs two_hundred_forty_nine_byte_name_hits_part_suffix_boundary`（双路断言，修复后走完成路径） |
| Hello 设备名控制字符注入（×2：ak-signal codec 与 ak-wire 显示字符串同规则） | 第七轮信令对抗子代理 | 第七轮信令代理（codec 层拒绝 C0/DEL/C1）＋ak-wire 同规则修复 | `ak-signal hello_with_control_characters_in_name_is_disconnected` 与 codec `err_name_with_control_characters`/`err_error_string_with_control_characters`；ak-wire 显示字符串规则测试锁定 |

## 如实边界

- 已验证范围＝本机环回（L2 形态）：单机、QUIC over UDP 环回/本机网卡、真实文件系统。**没有任何 L3/L4 验证被执行**——无真机、无跨网段/真实 NAT、无服务器端抓包（[TEST_PLAN](../docs/TEST_PLAN.zh-CN.md) 现状声明）。
- 环回吞吐是引擎自身效率对照，不代表真实网卡吞吐；多流收益环回仅 1.04–1.06x，是否默认启用待双机数据。
- 余项需外部资源：第二台真实机器（同网段双端实测）、真实网络环境（家宽/热点/受限 NAT，P0-05）、Flutter SDK 与移动工具链（P1-04）、控制面部署批准（P2-01）、真机支持矩阵（P0-01）与真机抓包审计（P0-07）。
- 已知限制见 [HANDOFF](../docs/HANDOFF.zh-CN.md) 与 SECURITY：接收端 Windows `available_bytes` 返回 None（空间不足以真实写错误暴露）、mtime 尽力回写、IPv6 前缀按 /64 简化、probe nonce 非密码学随机（身份由 QUIC 握手承担）、SameEgress 存在反代/CGNAT 误报场景。

## 下一步建议（与 HANDOFF"下一工程节点"一致）

1. 两台真实机器（同网段）跑 `ak-cli receive`/`send`，验证非环回路径与防火墙行为；随后 `signal-server`＋`probe` 在家宽/热点组合下实测公网直连成功率（P0-05，决定产品文案）。
2. 所有者确认 D2（公网失败提示文案）与 D3（端口映射默认开关）。
3. 视实测结果冻结网络库 ADR（评审 D1 尾项）并重估路线。
4. Flutter UI（Windows → Android 优先）与移动端后台/存储适配。

经第八轮复核上述节点顺序不变；本地可执行范围已尽，余项均需外部资源。
（2026-11-04 补记：上句为第八轮口径，已被其后进展更新——跨设备首证的模拟器形态已执行，本地可执行范围延伸并兑现；首要剩余项改为两台物理机器双端实测，详见 [HANDOFF](../docs/HANDOFF.zh-CN.md)"下一工程节点"与文末补章。）

## 第 10-16 轮补章（2026-11-04 补记）

九轮总结成文后，项目继续推进七轮（第十至十六轮）并取得首个跨设备里程碑。仓库现共 36 个提交（第九轮时为 20 个）。简表如下（关键数字取各轮验证记录与集成提交说明）：

| 轮 | 日期 | 主题 | 关键数字 | 提交 |
|---|---|---|---|---|
| 十 | 11-03 | ak-ffi C ABI 绑定层＋ADR-003 | 9 个导出函数、unsafe 20 块全隔离于 shim、6 项测试全经 C ABI（真实环回/坏 spec 反例/取消/无串扰/并发聚合） | `5851150`、`40d7d63` |
| 十一 | 11-03 | ak_ffi_dart 纯 Dart 包＋Flutter Windows 桌面端＋打包脚本 | dart test 69/69；flutter test 46/46；`flutter build windows --release` 成功（exe 90,624B＋ak_ffi.dll 4,205,056B 哈希一致）；打包 17 文件 zip 18MB | `b7c4a82`、`37cdf17`、`2875919`、`7dffa51`、`3dab426` |
| 十二 | 11-03 | 身份导出/簿准入/历史接线（ak-ffi→Dart→Flutter） | workspace 430/0；ak-ffi 13 项（identity_info/jobs_list/expect:book）；dart test 76/76；flutter test 59/59；打包 zip 18.1MB | `bc7fa01` |
| 十三 | 11-04 | 安卓平台移植＋Windows GUI 首启取证 | 双 ABI libak_ffi.so 交叉编译（x86_64 5,664,296B/arm64 5,557,928B，llvm-nm 符号验证）；flutter test 66/66；apk-debug.apk 161MB；GUI 首启截图视觉核验通过 | `834f42a`、`6ea65f0`、`1e6a8ab` |
| 里程碑 | 11-04 | 跨设备首证：Android 模拟器→Windows 宿主（CLI 路线） | Android ak-cli 交叉编译 8,904,736B 实跑；job `afe43780` 双端 complete 同 job id；2 条目 5.00 MiB `cmp` 逐字节一致；TOFU 记录宿主指纹；模拟器 NAT（10.0.2.2）形态，非物理双机 | `b4fb427` |
| 十四 | 11-04 | 配对链路：ak-ffi 三导出→Dart 绑定→Flutter 配对页→集成切换 | ak-ffi C ABI 23 项（peer +10）；dart test 85/85（peers +9 含 CLI 写簿往返）；flutter test 84/84（配对页 10 项；切换点升级真实 facade→C ABI→DLL 往返） | `1e6a8ab`（ffi 侧）、`f82ff77` |
| 十五 | 11-04 | daemon＋随机性质套件＋安卓存储通道 | ak-cli 83/83（daemon 7 项，NDJSON 五型事件流，双任务冒烟）；性质套件 25 种子传输＋10 种子中断续传不变量 release 94.8s 零缺陷（快速回归入默认 cargo test）；Kotlin SAF 写闭环通道、APK 重建 179MB | `f82ff77` |
| 十六 | 11-04 | 文档记录轮（本补章；BACKLOG/HANDOFF/TEST_PLAN/DEV_SUMMARY 四文件） | 不改代码；Rust 计数本机实测见下 | 不提交（按指示） |

### 当前状态一览（2026-11-04 实测/报告值，口径逐项标注）

- Rust workspace（2026-11-04 本机实测，`cargo test --workspace --no-fail-fast`）：**449 项通过、1 项 flaky 失败、7 项 ignored（默认运行合计 450 项）**——失败为 `ak-net stats_reflect_real_loopback_transfer`（第四轮既有测试，统计快照帧计数 68 对 66 的时序竞争；单独复跑 3/3 通过，属 flaky 非功能回归，未修复如实记录）。对账：第十二轮 430 ＋ daemon 7 ＋ ak-ffi peer 10 ＋ 性质快速回归 3 ＝ 450，与实测吻合（flaky 项在整 workspace 并行负载下偶发）。
- `#[ignore]` release 深测 7 项（5 项历史＋2 项性质套件变体）均执行通过；新增：property_random 25 种子随机树传输＋10 种子随机中断续传，release 94.8s 零缺陷。
- Dart（ak_ffi_dart）：85/85（集成轮报告值，本轮未复跑）；Flutter（flutter_app）：84/84（集成轮报告值，本轮未复跑）。
- 跨设备：Android 模拟器→Windows 宿主 CLI 组合 1 项，逐字节一致（模拟器 NAT 形态）；GUI 路线受阻于模拟器 System UI ANR（应用进程存活），物理双机/真实 NAT/抓包未执行。
- 仓库检查（L0）：`python scripts/check_repository.py` 本轮（2026-11-04）通过（banned_dependency_scan=clean、errors=[]）；clippy/doc 本轮未复跑（上一轮全绿记录见第九轮节，第十四/十五轮集成轮未见复跑记录）。

补章事实源：[BACKLOG](../docs/BACKLOG.zh-CN.md) 第十三至十五轮节与跨设备里程碑节、[HANDOFF](../docs/HANDOFF.zh-CN.md) 第十四/十五轮段、[TEST_PLAN](../docs/TEST_PLAN.zh-CN.md) 映射表、[开发验证记录](archive/dev-validation-2026-11-02.txt) 第十二轮验证/第十三轮进行中证据/跨设备里程碑节，及集成提交 `f82ff77` 说明。
