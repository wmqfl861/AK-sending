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

第十轮（2026-11-03，2 个并行子代理；证据见 reports/dev-validation-2026-11-02.txt"第十轮并行开发验证"节）：

- [x] ak-ffi C ABI 绑定层（ADR-003 契约先行）：9 个导出函数（句柄式/内嵌运行时/轮询进度 JSON/调用方缓冲区/spec JSON 未知字段忽略+控制字符拒绝）、错误码 0/1/2/-1..-5、catch_unwind 全入口；unsafe 20 块全部隔离于 shim 层逐块 SAFETY 注释（core 层 forbid(unsafe_code) 零 unsafe）；cdylib 产出 ak_ffi.dll 供前端动态加载。6 项测试全部经 C ABI 端到端：真实环回传输、20+ 坏 spec 反例、取消无假完成、双引擎无串扰、serve_concurrently 聚合。
- [x] ADR-003（FFI 边界决策与备选取舍：bridge 代码生成/JSON-RPC 子进程/CLI 管道）；SECURITY 增 FFI 边界节（契约层防护与已知限制）。

第十一轮（2026-11-03，Dart 层与工具链并行；证据：flutter_app/packages/ak_ffi_dart 全包 dart test 69/69、dart analyze 零告警）：

- [x] ak_ffi_dart 纯 Dart 包（不经 Flutter 工具链即可开发验证——Dart SDK 随克隆就位）：FFI 绑定层（9 函数 dart:ffi、Dart 侧 spec 校验镜像 Rust、-3 自动扩容、Finalizer 防泄漏；真实 DLL 环回/取消/-3 语义 9 项测试）+ 模型层（19 事件类型化+Unknown 兜底、TransferReport、与 ak-cli 逐字对齐的 humanBytes/两行式指纹/进度条、准入策略解析——配对码匹配显式留给 Rust 不在 Dart 做哈希）+ 状态层（JobController 相位机/多连接聚合/文件表，FFI-free 59 项测试）。
- [x] Windows 打包脚本 scripts/package_windows.ps1：exe+DLL+README+可选 Flutter 产物（探测式接入）+SHA256 MANIFEST+zip，两次自测幂等；仓库检查脚本同步排除 target/（dist 在场验证绿）。
- [x] Flutter 桌面端 UI（2026-11-03 终波，P1-04 的 Windows 桌面首端）：flutter test 46/46（Fake 时间线驱动的页面/组件测试 + 真实 DLL 服务冒烟）；`flutter build windows --release` 实际成功（exe+ak_ffi.dll 哈希一致）；五页 GUI（身份卡/接收/发送/历史/关于），连通性失败文案对齐 UX 文档；TransferService 抽象使 UI 全可测；打包脚本接入 → 完整产物 17 文件（CLI+DLL+GUI）zip 18MB。第十二轮（2026-11-03，3 代理接力）补齐上轮已知限制：

- [x] ak-ffi `ak_identity_info`（load_or_create，返回 device_id/指纹/配对码/簿计数/created）与 `ak_jobs_list`（检查点列表 JSON；带 id_dir 任务检查点落 `<id_dir>/checkpoints` 与 CLI 同构）+ receive `expect:"book"` 簿准入（每连接重读 peers.book，空/坏簿启动即错，与 CLI 语义一致）——13 项 C ABI 测试。
- [x] ak_ffi_dart 接 identityInfo/jobsList/book（真实 DLL 76/76：簿准入 e2e 接受配对端、空簿 "pair first"、指纹互 pin 环回即证一致性）。
- [x] Flutter 接线：身份卡真数据（created 提示/已配对设备数）、book 准入直达引擎（错误透传）、历史页真实列表（peer 短式+root）；Facade 切换真实调用；flutter test 59/59 + build windows 成功 + 打包含 UI。剩余如实：发送侧前缀→簿补全仍本地拒绝（后绪项）。

第十三轮（2026-11-04，安卓移植波，提交 `834f42a`＋`6ea65f0` 证据记录；证据见 reports/dev-validation-2026-11-02.txt"第十三轮进行中证据"节与 docs/ANDROID_BUILD.zh-CN.md）：

- [x] 双 ABI 交叉编译脚本＋符号验证（安卓构建侧前置）：`scripts/build_android_ffi.sh` 纯环境变量 linker 方案（不新增 `.cargo/config.toml`），一次构建 x86_64-linux-android＋aarch64-linux-android（NDK 28.2，Windows 主机），产物复制进 jniLibs 并以 NDK `llvm-nm -D --defined-only` 抽查冻结符号（ak_engine_new/ak_identity_info/ak_jobs_list）、target 与 jniLibs 两处 SHA-256 一致。产物哈希：x86_64 5,664,296 字节 `6d92792e546fb6f085723d42883e5ea599ee1c0fde8d3e5c4003d54cc392e7a2`；arm64-v8a 5,557,928 字节 `c7f1eaa04bbeb9b55e288af0b502e26d78bf8e6a91914c0dd29168c5026a58cb`。符号验证属静态产物检查，不等价真机运行验证（ANDROID_BUILD 文档标记约定）。
- [x] flutter_app 安卓平台（P1-04 安卓首端；P0-06 代码侧）：INTERNET 权限（LAN 直传必需，release 只从 main manifest 获得）、minSdk 24（与 NDK clang 的 android24 目标三元组对齐）、tier-0 `libak_ffi.so` 加载（System.loadLibrary 语义，桌面三级 DLL 探测全部跳过）、path_provider 身份目录异步解析（`<应用支持目录>/.ak-sending`，首次引擎读取前等待解析完成）、应用专属 Download 输出链（getDownloadsDirectory → getExternalStorageDirectory+`Download/` → 文档目录+`Download/`；公共 Downloads 导出留待 SAF/MediaStore 后续波次）；flutter test 66/66、analyze 零告警。
- [x] APK 构建成功且双 ABI so 在包内：apk-debug.apk 161MB（debug 形态，未做 size/strip 优化），libak_ffi.so 双 ABI 随包（包内 x86_64 4.3MB＋arm64 3.7MB）；装机当时尚未执行（其后随跨设备里程碑完成，见下节）。
- [x] Windows GUI 首启截图核验（第十一/十二轮 GUI 的真启动补证）：target/windows_gui_first_launch.png 经视觉模型核验，五页导航与身份卡完整渲染（配对码 146-778 大号展示、指纹 b6998f27），无白屏/崩溃/错版；主题实测浅色、与任务描述不符处如实记录。
- [x] AVD/模拟器装机（收口于跨设备里程碑，见下节）：系统镜像带校验重试重装完成、模拟器就绪；CLI 与 GUI 两种 APK 均装机成功；跨设备传输经 CLI 路线打通（逐字节一致）。
- [ ] 模拟器 UI 驱动验证（截图/操作走查）：GUI 装机成功但模拟器 System UI 不稳（三轮 ANR；应用进程存活＋topResumed 确认），GUI 截图受阻——如实记录，CLI 路线已提供跨设备证据（见下节）。
- [x] 跨设备实测（模拟器↔主机部分）：见下"跨设备里程碑"节；两台真实机器（物理双机）仍未执行（P0/P3 余项）。
- [ ] 安卓 mDNS 发现、公共目录导出选择器走查、后台/生命周期适配（P0-06 剩余部分）：SAF 存储通道代码已落地（第十五轮，Kotlin `dev.ak/storage` 四方法含 SAF 写闭环探针，APK 179MB 重建）；模拟器端 SAF 选择器走查待 GUI 路线恢复后执行。

跨设备里程碑（2026-11-04，Android 模拟器→Windows 宿主真实 QUIC 传输，CLI 路线；提交 `b4fb427` 证据记录，reports/dev-validation-2026-11-02.txt"跨设备里程碑"节）：

- [x] 首个跨设备传输组合：`ak-cli` 交叉编译 x86_64-linux-android（8,904,736B；ring/rustls 的 C 构建沿用 build_android_ffi.sh 的每目标 `CC_x86_64_linux_android`/`AR_x86_64_linux_android` 指向 NDK clang/llvm-ar）→ adb push 至模拟器 `/data/local/tmp` → Android 端 `ak-cli --version` 实跑确认（wire v1、caps 0x0007、release）→ Windows 宿主 receiver（`--expect any`，端口 17900）↔ Android 发送端经 `10.0.2.2`（模拟器→宿主 NAT 环回别名）。job `afe43780da634ea94e8e7e6ec97da314` 双端 complete 且同 job id：2 条目 5.00 MiB（5,242,880B 随机 payload.bin＋中文文件名"说明.txt"）；`cmp` 逐字节一致（payload.bin/note.txt 均 IDENTICAL）；QUIC＋TLS 指纹钉扎握手跨设备真实成立；Android 端 TOFU 记录宿主指纹 499f62ba，keygen/peers.book/device.key 全功能。层级如实：真实双进程＋跨网络栈（模拟器 NAT 10.0.2.2），非进程内环回；**仍非物理双机**（模拟器与宿主同机），真实 NAT/公网（P0-05）与抓包（P0-07）未执行。宿主侧配套脚本 `scripts/smoke_android_host.sh`（提交 `1e6a8ab`；`--self-test` 为无设备门槛；Android→宿主方向经 `--fp` 钉扎＋配对码目视核对的安全取舍脚本内如实声明）。

第十四轮（2026-11-04，配对链路波：ak-ffi 三导出→Dart 绑定→Flutter 配对页→集成者切换；ak-ffi 侧随 `1e6a8ab` 提交，其余随 `f82ff77` 集成提交）：

- [x] ak-ffi 配对簿三导出（`ak_peer_list`/`ak_peer_add`/`ak_peer_remove`，crates/ak-ffi/src/shim.rs，schema 冻结于 lib.rs）：缺簿＝`[]`、坏簿＝`AK_ERR_INVALID`（"cannot load peer book"）、列表按指纹排序；add 即 CLI `pair add` 语义——新对等端无名存 `"(unnamed)"`、upsert 改名保原 `paired_unix_ms`、`AK_ERR_BUFFER` 重试幂等、坏指纹与控制字符名 `-2` 拒绝；remove 未知指纹 `-4`、簿原子保存；写路径先试探缓冲再落簿，保存失败 `AK_ERR_INTERNAL` 且簿原样。C ABI 测试 23 项全经导出函数驱动（peer 新增 10 项：缺/空簿、CLI 写簿排序解析、坏簿、add 默认名/upsert 保时/坏输入拒绝、remove 删除与 `-4`、identityInfo 计数联动、预写缓冲、保存失败簿无损）——`cargo test -p ak-ffi`（tests/ffi.rs）23/23。
- [x] ak_ffi_dart peers 绑定（packages/ak_ffi_dart/lib/src/peers.dart＋bindings/engine 扩展）：`PeerEntry` 模型（fingerprint/name/pairedUnixMs）与三方法 Dart 侧镜像校验（64 hex、`-3` 自动扩容无损重试、`-4` 异常化）；dart test 85/85（peers 新增 9 项：缺簿 `[]` 且零落盘、坏簿 `-2` 不破坏、CLI 写簿往返、upsert 保配对时间、坏指纹/控制字符名、remove 删除/`-4`/坏指纹、identityInfo 簿计数联动）。
- [x] Flutter 配对页（flutter_app/lib/ui/pages/pairing_page.dart）：簿列表（未命名回退文案、指纹短式 chip 点击展开完整 64 hex）、添加表单（非 hex 内联拒绝；8–63 位前缀按冻结契约提示需完整 64 hex——前缀解析是 CLI 发送侧能力，簿写入要求完整指纹）、删除二次确认（取消保留/确认删除/对端已消失 false 分支）、坏簿内联错误行不崩页；主页"已配对设备 N 台"随簿联动刷新（home_page/app 接线）。flutter test 84/84（配对页 10 项 widget 测试；TransferService 扩 listPeers/addPeer/removePeer，Fake 同步实现）。
- [x] 集成者切换与真 DLL 升级：`AkEngineDartFacade` peer 三方法由 `UnsupportedError` 桩切为真实 `engine.peerList/peerAdd/peerRemove` 调用（预留一行切换点兑现）；切换点测试升级为真实 facade→C ABI→ak_ffi.dll 往返（services_test.dart "the real AkEngineDartFacade peer seam drives the real DLL (wave-14 integration switched on)"）；84/84 复测通过。

第十五轮（2026-11-04，daemon＋随机性质套件＋安卓存储通道；提交 `f82ff77`）：

- [x] `ak-cli daemon` 常驻接收（crates/ak-cli/src/daemon.rs）：单进程三关切——与 `receive` 共享 `ReceiveLoopConfig` 的身份/准入（两动词解析一致性测试钉死，语义不可漂移）、mDNS 广播（指纹 TXT，与 `discover` 同一记录）、顺序接收循环（单任务失败/超时不终止进程，报告后继续监听，Ctrl-C 撤广播退 0）；`--json-lines` 输出 NDJSON 事件流（listening/job_started/job_done/error/stopped 五型；字段名为脚本面向契约只增不改，调用方字符串全经 json_escape，人读笔记走 stderr 保持 stdout 逐行可解析）。ak-cli 83/83（daemon 单元 7 项：默认/全量旗标解析、NDJSON 精确单行渲染、字符串 JSON 转义、设备名校验回退 whoami、与 receive 身份/准入解析一致、坏 conflict/expect 拒绝）；真实双任务冒烟（同一 daemon 进程连续接收两个 job，事件流完整）。
- [x] 确定性随机性质套件（crates/ak-itests/tests/property_random.rs＋prop_common.rs）：一切随机出自无依赖 LCG（Knuth 常数），种子完全决定树形/字节/旋钮，断言失败消息带 seed＋生成器摘要、同种子重跑可复现。两套 release 深测（`#[ignore]`，本轮执行 94.8s 零缺陷）：25 种子随机树传输不变量（0-40 文件、单文件 0-512KiB、深 0-6、CJK/内部空格/多点/点前缀/单字符混合名、随机空目录链、chunk_kb∈{16,64,256}、multistream 随机——双端 Report 计数对账、job_id 双端一致、全新目的地 resumed=0、ChunkReceived 恰每 chunk 一次、逐字节一致、无 .akpart 残留）＋10 种子随机中断续传不变量（中断点由第二种子流抽取；leg-1 发送端必须失败关闭、中断仅数据面——0 最终文件/≥1 part 存活/恰一条检查点、同身份重传 resumed≥1 且 job_id 与中断前一致、逐字节一致）。快速回归（种子 1/2/3 缩小版）入默认 `cargo test`。已知限制模块文档声明（全空树/多根不在矩阵、中断为进程内 abort 形态非真实断网注入、"最后一刻完成"竞态以 leg-1 失败关闭断言兜底）。
- [x] 安卓存储通道（flutter_app/android/app/src/main/kotlin/dev/ak/ak_sending_app/MainActivity.kt，MethodChannel `dev.ak/storage`）：`getStorageInfo`（应用专属 Download 目录＋卷剩余字节＋SAF 持久授权态）、`pickFolder`（ACTION_OPEN_DOCUMENT_TREE；FlutterActivity 非 ComponentActivity，自建 ActivityResultRegistry 经 v2 embedding 路由，无弃用 onActivityResult）、`safWriteTest`（DocumentsContract createDocument→写 1 字节→读回校验→delete，SAF 写路径最小闭环探针）、`listSafPersisted`（uri/read/write/tree/persistedTimeMs）；选择器进行中重复触发与坏 URI 反例处理；APK 重建（apk-debug.apk 179,026,123B，2026-11-04 10:15，debug 形态）。Kotlin 文件头如实声明：本轮保证编译通过与通道闭环设计，模拟器/真机 SAF 选择器走查与 Flutter 侧接线为其后续（Android TV 无选择器返回 null、SAF 授权可被系统收回等限制同处声明）。

## 邀请码配对功能（第十八至二十一波，2026-11-04）

前置：Codex（gpt-6-astra/max）只读产出两批开发计划（`docs/plans/pairing-code-plan-draft.md`＋`pairing-code-plan-part2.md`，提交 `a07d8c1`/`7a5f50e`）。按 AGENTS.md 并行开发规则第 3 条（公共 API 契约先行冻结再并行），第十八波先把全部跨模块接口冻结为 ADR-004＋`docs/contracts/` 契约，第十九至二十一波按冻结契约多代理并行实现。决策与备选取舍见 [ADR-004](adr/004-invite-pairing.zh-CN.md)；证据见 [dev-validation-2026-11-02](../reports/dev-validation-2026-11-02.txt)"第十八/十九波""第二十波""第二十一波"三节；提交链：`1e8b276`（第十八/十九波，5 并行代理）→ `18f2aae`（第二十波，4 并行代理）→ `bfc74d7`（第二十一波，2 并行代理＋集成者修复）。

- [x] A0 契约冻结（第十八波）：ADR-004 与 `docs/contracts/` 五份契约（protocol-v2/pairing-flow/scheduler/ffi-v2/storage-schema，合计六份冻结文档）共 157 项与 Codex 计划逐字核对通过；修改任一契约需集成者核准并同步 ADR-004。
- [x] A1 控制协议 v2 codec（第十九波，`crates/ak-signal/src/protocol_v2/`）：30 个 `CtrlMessage` 变体、逐类型 payload 预算锁定、方向表穷举、封闭 tag 集、尾随字节/超长/截断拒绝——47 项测试与契约零出入。
- [x] A4 邀请码与信任簿事务（第十九波，`crates/ak-identity`）：`InviteCode`（`OsRng` 20 字节 160 bit 熵、Crockford Base32、`AKI1-` 展示格式、lookup/auth 域分离派生、Debug 脱敏）、typed transcript 五查（换钥/换码/过期/nonce 重放/MAC 恒定时间）、`PeerBookStore` 事务式落簿与防重放——新增 34 项测试，crate 73/73。
- [x] A5＋B1 本地持久化（第十九波，`crates/ak-store`）：`PairingJournal` 四态单调（pending→committed→applied＋Ready 边界）、`TaskQueueStore` AKREC2 恢复映射（`transfer_id` 域分离、AKREC1 兼容）、三类崩溃模拟（每个落盘边界崩溃恢复/坏记录失败关闭/不污染文件检查点）——默认特性 38 项＋sqlite 特性 54 项。
- [x] B0 共享调度层（第十九波，新 crate `ak-session::scheduler`）：same_code×cross_code 四模式矩阵、同组 FIFO＋组间轮转公平、parallel→serial draining、permit 准入、队列满前不扫描——51 项测试。
- [x] 第十八/十九波收官：`cargo test --workspace` 667/0（基线 507 → +160）；clippy `-D warnings` 干净；sqlite 特性 54/54；repo check passed。
- [x] A2 TLS 注册服务（第二十波，`crates/ak-signal/src/server_v2/`）：rustls＋tokio-rustls TLS 1.3（TLS exporter 按契约原样实现）、SQLite 三表（`invite_codes` 等与 storage-schema 契约逐字）、claim `BEGIN IMMEDIATE` 线性化、ghost-pending 统一 `PAIR_UNAVAILABLE`（无存在性预言机）、claim→redeem→Ready 全时序、限速双桶、重启恢复（码＋挂起事务＋状态查询重发）——63 项新测试（`server-persistence` 特性下 ak-signal 227 全绿）。
- [x] A3 v2 客户端分派（第二十波，`crates/ak-signal/src/client_v2/`）：TLS 钉扎、单 reader 有界分派（修复 v1 无界 inbox 形态）、`request_id` 幂等重试、重连 epoch 重置、被替换通知——25 项集成＋9 项单元。
- [x] B6 常驻 runtime（第二十波，`crates/ak-session/src/runtime/`）：SessionHandle 七 API（start/enqueue/set_policy/cancel/retry/snapshot/shutdown）、PairingTransport/TransferExecutor 双 trait 注入、四模式以任务活动时间区间证明、退避 park＋手动 retry、Shutdown Drain/Cancel/超时——28 项新测试（ak-session 累计 79）。
- [x] B2 wire 任务准入＋B4 staging（第二十波）：`ak-wire` 准入三消息（TaskOpen/TaskAdmission/TaskAdmissionAck，`CAP_SCHEDULER` 0x0008，封闭 reason）＋`ak-storage` staging 命名空间（同目标并发按内容裁决、旧 `.akpart` 迁移兼容、取消隔离）——新增 22 项测试（波末 crate 合计 wire 59/storage 48）。
- [x] 第二十波收官：`cargo test --workspace` 773/0（667 → +106）；sqlite 54；ak-signal＋持久化特性 227；clippy 干净；repo passed。
- [x] A6 配对编排 e2e（第二十一波，`crates/ak-session/src/pairing/`）：`PairingOrchestrator` create（CODE_CONFLICT 重试）/redeem/recover/revoke 全时序＋`RealTransport`；13 项真 TLS＋SQLite e2e（全流程互录 journal/owner 离线补推/中断恢复/防中间人（错 auth_key 不落簿）/同码多兑换/同 `request_id` 幂等/双 runtime listener 收真事件）；发现并报告 A2/A3 三处常量分歧（ALPN/pin/exporter label），`RegistryClient` 按服务端公共面构建绕过。
- [x] B5 引擎拆分（第二十一波，`crates/ak-engine`）：`send_on_channel`/`receive_on_channel`（准入三步握手＋staging），既有 49+1 零回归，新 8 项测试。
- [x] B7 CLI（第二十一波，`crates/ak-cli`）：`invite create/redeem/list/revoke`＋`registry-server`（TLS＋SQLite，默认端口 7200）＋`daemon --invite-code/--same-code/--cross-code`——12 项新测试。
- [x] 第二十一波集成收口：ak-net type_budget/header_payload_check 测试适配新 wire 类型 13–15；invite.rs clippy 三处修复。收官：`cargo test --workspace` 851/0（773 → +78）；clippy 干净；repo check passed。
- [ ] B8 FFI 接线（`ak_pairing_*`/`ak_sched_*` 18 个新导出＋Dart 绑定＋Flutter 入口）：契约已冻结于 `docs/contracts/ffi-v2.md`；截至 `bfc74d7` 未实现，按计划属第二十一至二十二波。

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
