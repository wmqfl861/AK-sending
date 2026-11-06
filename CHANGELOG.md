# 变更日志

本文件记录用户可感知的主要里程碑，按时间线组织；逐轮测试证据见 `reports/dev-validation-2026-11-02.txt`，任务级完成状态见 [docs/BACKLOG.zh-CN.md](docs/BACKLOG.zh-CN.md)，平台支持范围见 [docs/SUPPORT_MATRIX.zh-CN.md](docs/SUPPORT_MATRIX.zh-CN.md)。格式参照 Keep a Changelog；版本号语义为预发布（0.x）。

## [未发布] - 2026-11-06：第二十七波——daemon 真实邀请码两维并发（终审 P1-02 关闭）

- 引擎：`channel_scheduler_handshake`（Hello 能力分流 + TaskOpen 读取）与 `receive_on_channel_preadmitted`（预准入接收体）；Legacy 分支带回流（`HelloStream`）+ `receive_after_hello` 同连接延续体；`receive_job` 零复制重构为三阶段共享。
- 会话层：`ChannelReceiveExecutor`（准入接收执行器）与 `AdmissionValidator`（事实源 = pairing journal Applied 记录；组键只从关系重建，claim 字节仅比对——伪造 code_id 换队列被拒）。
- daemon：单次 bind + 并发 accept + 每连接 handler；Scheduler 对端按真实组键入队（同码/跨码策略矩阵在生产路径生效），legacy 对端同连接旧协议；关停 Drain 语义保持。
- 发送端：CLI send 在 id-dir 有匹配 Ready 关系且对端支持时自动走 TaskOpen 准入（拒绝报 BUSY 不静默降级；旧接收端显式回退）。
- 集成修复（联调暴露）：daemon 全程共享一条注册连接（v2 每设备单连接，双连接互踢曾致 redeem offer 推给无人读的连接）；orchestrator 新增 `adopt_persisted_code`（跨进程持久码回填）；生产 `--pairing` 切换到 orchestrator owner 环（写 journal，准入校验的事实源）；老 owner 循环死代码清理。
- 证据：同码两 redeemer 并发→接收区间不重叠；异码并发→重叠；双端策略交集（一端 serial 即串行）；准入拒绝→恰 1 个 TaskOpen、零落盘、无静默重试；6/6 多进程真链 itest 全绿。

## [未发布] - 2026-11-06：第二十六波——二次终审 20 项发现修复（P0×1 / P1×6 / P2×13）

- P0-01 FFI 快照 ABI 错配（真实 GUI 可触发原生堆越界写）：ABI 裁决冻结为实际发布形态（snapshot 三参无 runtime_id/query），契约重写 + Rust fn 指针形状测试锁定 19 导出 + Dart 绑定修正——两侧再漂移即编译失败。
- P1-01 连接预算与文件扫描分离（引擎 Connecting 信号起算）；实测将预算 3s→10s（跨公网 VPN 握手需多轮 RTT，3s 在真实环境双版本复现必失败）。
- P1-03 真实引擎任务纳入 runtime 关停生命周期（可取消可等待句柄、Drain 超时先 join 再答、取消与最终提交竞争以持久状态为准）；P1-04 配对停止竞态（终态原子先行，迟到连接不再复活 runtime）；P1-05 Dart dispose 悬空 native 调用（四步释放序列+waiter 失效）；P1-06 调度 enqueue 补必填 peer（Dart↔Rust↔契约三侧对齐）。
- P2×13：调度限额收敛/跨码 serial 有限批次/策略三限额贯穿/enqueue request_id 幂等/历史有界化/原生错误详情传播/IPv6 候选族匹配 bind/候选尊重实际监听地址/跨设备脚本失败现场保留+公网回退+IPv6 端点格式化/检查脚本退出码分类/五份状态文档口径统一。
- 验证：workspace 995/0 + clippy 干净 + 契约 5/5；dart 112/112、flutter 111/111；跨设备复跑 6/6 PASS（W→L 8MiB 3.8s 含真实候选回退、L→W 8MiB 14.6s，双向 SHA256 一致）。

## [v0.1.0] - 2026-11-05

从零到邀请码配对全链：2026-11-02 至 2026-11-05，24 个开发波次（含两轮外部 Codex 只读评审及其修复波），13 个 Rust crate ＋ 1 个 Dart 包 ＋ Flutter 桌面/移动应用。

### 2026-11-02：立项与决策基线

- ADR-001 冻结"纯直连、无中继、无云端暂存"不可妥协约束；ADR-002 冻结协议栈与开发顺序（数据面 QUIC＋自建信令＋最小 STUN，iroh 因打洞依赖 relay 被排除）。
- 需求/架构/协议/安全/测试计划等全套中文文档就位；Rust 工具链首次实编译验证。

### 2026-11-02/03：局域网 CLI 原型（第一至二轮）

- 11 crate 模块化骨架（identity/wire/storage/net/discovery/store/signal/engine/cli/itests）：Ed25519 设备身份＋指纹＋配对码＋自签证书钉扎；QUIC 直连数据面；mDNS 发现。
- 传输内核：文件清单/BLAKE3 全量与分块摘要/mtime 源版本检测；有界分块读写；`.akpart` 原子落盘与透明续传；检查点重启恢复；5 GiB 大文件与 20,000 海量文件实测通过。
- CLI 产品化第一版：配对簿准入、任务管理、接收循环、`--multistream` 多流并行。

### 2026-11-03：CLI 原型深化（第三至八轮）

- 存储满预检与真实余量告知、多根合并发送、同出口提示、空闲超时、Pause/Resume、字节级进度。
- ResumeRequest 多帧分页（解除单帧条目上限，65,537 文件边界实测）、SQLite 检查点后端（可选特性）、并发接收、IPv6 双栈端点。
- 对抗加固：信令 500 帧模糊、STUN 畸形包洪泛、身份替换/端口抢占/重钉扎场景全部自动化钉死；248 字节长名/CJK/emoji 等极端名边界与 NTFS 长 part 名回退。
- `ak-cli doctor` 环境自检、probe 公网直连探测工具、bench 吞吐与块尺寸基准、completions、`--json` 系列输出。
- 首个产品全流程 e2e：配对准入→中断→续传→并发→信令死亡直传照常。

### 2026-11-03：FFI 与 Windows 桌面 GUI（第十至十二轮）

- ak-ffi C ABI 绑定层（ADR-003：句柄式、JSON spec、轮询进度、catch_unwind 全入口、unsafe 全隔离于 shim 层）。
- ak_ffi_dart 纯 Dart 包（FFI 绑定＋19 事件类型化模型＋JobController 状态层）；Flutter Windows 五页 GUI（身份/接收/发送/历史/关于）release 构建成功并打包；身份/任务/配对簿导出与 UI 全接线。

### 2026-11-04：安卓移植与首个跨设备里程碑（第十三至十六轮）

- 双 ABI（x86_64/arm64）交叉编译脚本＋NDK 符号验证；Flutter 安卓平台（minSdk 24、应用专属 Download 输出链）；APK 构建与 AVD 装机。
- **首个跨设备传输**：Android 模拟器→Windows 宿主真实 QUIC＋TLS 指纹钉扎传输，逐字节一致；随后补齐宿主→安卓反方向。
- `ak-cli daemon` 常驻接收（NDJSON 事件流）；确定性随机性质套件（25 种子随机树＋10 种子随机中断续传，release 深测零缺陷）；安卓 SAF 存储通道（Kotlin MethodChannel）；桌面真实应用级 E2E（真实窗口进程全绿）；跨平台目标矩阵脚本。

### 2026-11-04：邀请码配对（第十七至二十一波）

- 第十七轮：首轮外部 Codex 评审的全部发现（2 P1＋14 P2）修复关闭。
- ADR-004＋六份冻结契约先行；控制协议 v2 codec（30 消息变体、封闭 tag 集）；邀请码 `AKI1-`（160 bit 熵、域分离派生、Debug 脱敏）与 typed transcript 五查（换钥/换码/过期/重放/恒定时间 MAC）。
- TLS 注册服务器（rustls TLS 1.3＋SQLite、claim 线性化、防枚举、限速、重启恢复）；v2 客户端分派（钉扎＋幂等重试）；共享调度层（same-code/cross-code 四模式、组间公平）；常驻 runtime；wire 任务准入＋staging 命名空间。
- 配对编排 e2e（create/redeem/recover/revoke 全时序、防中间人、owner 离线补推、中断恢复）；引擎 `send_on_channel`/`receive_on_channel`；CLI `invite` 命令族与 `registry-server`。

### 2026-11-04/05：配对进前端与终审修复（第二十二至二十三波）

- 第二十二波：FFI 配对导出→Dart 绑定→Flutter 邀请页→集成测试→构建矩阵。
- 第二十三波：外部 Codex 终审发现修复（5 P1 全关；2 项 P2 边界如实保留开放——on-channel 准入前扫描、检查点 AKREC1 实例隔离，见 BACKLOG"终审发现"节）——恢复路径 digest sidecar 钉死（恶意 registry 换钥无法通过）；staging 符号链接 TOCTOU 收窄＋父目录防护＋保留命名空间隔离；并发提交"一次提交、一次复用"内容裁决（barrier 并发测试）；准入上下文私有化＋wire 字段补全＋通道指纹复核＋fail-closed；注册服务器资源界限（连接上限/超时/读写截止）；补齐 5 个缺失 FFI 导出（v2 导出合计 19 个，双 ABI 符号验证）。

### v0.1.0 收官形态

- **数据面**：QUIC＋TLS 指纹钉扎直连，无任何中继回退；单/多流、断点续传、暂停恢复、IPv4/IPv6 双栈。
- **身份与配对**：Ed25519 设备身份、配对簿（手动指纹/前缀）、mDNS 发现、一次性邀请码（create/redeem/revoke/TTL）经自托管 TLS registry。
- **存储**：有界内存、BLAKE3 摘要、原子 no-replace 提交、staging 命名空间、文件/SQLite 双检查点后端、海量文件分页。
- **前端**：Windows 桌面 GUI（五页＋配对簿＋邀请页）与 Android 应用（双 ABI）；ak-cli 全功能命令行；ak-ffi 19＋14 导出 C ABI。
- **质量**：workspace 918 项测试全绿、clippy `-D warnings` 干净、确定性随机性质套件、双外部评审全关闭。

### v0.1.0 已知限制（开放项）

- 生产双端调度接线进行中（CLI daemon 调度旗标仍走策略模型＋Fake executor，第二十四波接线中）。
- 物理双机、真实 NAT/公网、真机抓包未执行（需外部网络与设备）；模拟器 GUI 走查受阻于 System UI 稳定性。
- Linux/macOS/iOS：Linux 仅类型检查与 runner 就绪；macOS/iOS 由所有者明确排除。
- 逐项开放清单见 [docs/BACKLOG.zh-CN.md](docs/BACKLOG.zh-CN.md)。
