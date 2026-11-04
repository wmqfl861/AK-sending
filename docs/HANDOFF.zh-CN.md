# 当前交接

## 目标与授权

仓库：`wmqfl861/AK-sending`；默认分支：`main`；本地开发分支：`dev/local-parallel`（未推送）。
用户已授权上传本项目全部文档和代码，并完成目标账户连接。
服务器仅可用于设备发现、信令和 NAT 穿透协调，绝不承载文件数据或密文；ADR-001 不变。
2026-11-02（第二阶段）：用户指示"继续本地开发，多模块多子代理并行，直到项目完成，不用推送"；本地分支推进，未推送、未部署、未强推、未加许可证。

## 第二至十五轮完成内容（2026-11-02 至 2026-11-04，dev/local-parallel）

按 ADR-002（采纳评审 D1/D5/D6 路线）完成局域网 CLI 原型全栈与桌面/安卓前端：

- 提交历史：`c13ecb8`（脚手架/接口冻结）→ `925c63d`（六模块并行实现）→ `4a3bf36`（引擎+CLI 集成与验证）→ `911bc31`（第二轮：引擎增强/配对 CLI/存储补强/场景测试）→ `5ac6f2a`（第三轮：四并行代理＋集成者修复）→ `0140282`（第四、五轮：十并行代理＋接线）→ `9961506`（65,537 边界补记）→ `ab33a46`（第六、七轮：六并行代理＋集成者修复）→ `d1fa222`（六/七轮 BACKLOG 记录）→ `3b39c03`（第八轮：产品流程 e2e/CLI 打磨/文档对齐）→ `cbdb2a4`（第八轮 BACKLOG 记录）→ `64c7278`（第九轮：文档收尾/CI 配置/冒烟脚本）→ `40d7d63`（第十轮：ak-ffi C ABI＋ADR-003）→ `7dffa51`（第十一轮：Flutter Windows 桌面端）→ `bc7fa01`（第十二轮：身份导出/簿准入/历史接线）→ `834f42a`（第十三轮：安卓平台移植）→ `6ea65f0`/`1e6a8ab`（第十三轮证据记录＋重启恢复手册；后者随附 ak-ffi peer 三导出与 smoke_android_host.sh）→ `b4fb427`（跨设备里程碑证据记录）→ `f82ff77`（第十四/十五轮集成：配对 UI 真调用、daemon、随机性质套件、安卓存储通道）。
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

第十三轮新增（2026-11-04，安卓移植波，提交 `834f42a`＋`6ea65f0` 证据记录；第十至十二轮——ak-ffi C ABI/ak_ffi_dart/Flutter 桌面端接线——逐项见 [BACKLOG](BACKLOG.zh-CN.md)，此处不重复）：`scripts/build_android_ffi.sh` 双 ABI 交叉编译（纯环境变量 linker、NDK 28.2、`llvm-nm` 冻结符号验证、jniLibs 与 target 哈希一致，流程与故障排查见 [ANDROID_BUILD](ANDROID_BUILD.zh-CN.md)）；flutter_app 安卓平台（INTERNET 权限、minSdk 24、tier-0 so 加载、path_provider 身份目录异步解析、应用专属 Download 输出链），flutter test 66/66；apk-debug.apk 161MB 构建成功、双 ABI so 在包内（装机尚未执行）；Windows GUI 首启截图核验通过（配对码/指纹渲染正常，证据见验证记录"第十三轮进行中证据"节）。进行中：AVD 系统镜像重装（首次下载 zip 损坏已定位，unknown archive @33%）→ 模拟器装机与跨设备传输。环境事实：Android SDK/NDK/JDK 布局于 `~/android-toolchain`（`sdk/ndk/` 下 27.2 与 28.2 双版本，脚本自动取最高），Flutter SDK 于 `~/flutter-sdk`（使用前 `export PATH="$USERPROFILE/flutter-sdk/bin:$PATH"`）；模拟器操作经 android-emulator 插件的 23 个工具以 JSON-RPC 直驱 MCP 服务器（约定辅助脚本 `/tmp/mcp_call.py` 发起调用）。

跨设备里程碑（2026-11-04，提交 `b4fb427` 证据记录；reports/dev-validation-2026-11-02.txt"跨设备里程碑"节）：Android 模拟器→Windows 宿主真实 QUIC 传输打通（CLI 路线）——`ak-cli` 交叉编译 x86_64-linux-android（8,904,736B；ring/rustls 的 C 构建经每目标 `CC_*`/`AR_*` 指向 NDK clang/llvm-ar）实跑于模拟器，宿主 receiver（`--expect any`，端口 17900），发送端经 `10.0.2.2`（模拟器→宿主 NAT 环回别名，非物理双机）。job `afe43780da634ea94e8e7e6ec97da314` 双端 complete、同 job id，`cmp` 逐字节一致（5.00 MiB 随机 payload.bin＋中文文件名"说明.txt"）；Android 端 TOFU 记录宿主指纹 499f62ba。层级如实：真实双进程＋跨网络栈，仍非物理双机。GUI 路线受阻如实记录：APK 装机成功但模拟器 System UI 三轮 ANR（应用进程存活＋topResumed 确认），GUI 截图/走查未成——CLI 路线先行提供跨设备证据。

第十四轮新增（2026-11-04，配对链路波；ak-ffi 侧随 `1e6a8ab`，其余随 `f82ff77`）：ak-ffi 配对簿三导出 `ak_peer_list`/`ak_peer_add`/`ak_peer_remove`（缺簿 `[]`、坏簿 `-1`、upsert 保 CLI TOFU 配对时间、坏指纹/控制字符名 `-2`、remove 未知 `-4`；C ABI 测试 23/23）；ak_ffi_dart peers 绑定（PeerEntry 模型＋`-3` 无损重试，dart test 85/85 含 CLI 写簿往返）；Flutter 配对页（簿列表/指纹短式展开/添加表单完整 64 hex 校验/删除二次确认/坏簿内联错误行，flutter test 84/84）；集成者将 `AkEngineDartFacade` peer 三方法由桩切为真实调用，切换点测试升级为真实 facade→C ABI→DLL 往返。逐项证据见 [BACKLOG](BACKLOG.zh-CN.md) 第十四轮节。

第十五轮新增（2026-11-04，提交 `f82ff77`）：`ak-cli daemon` 常驻接收（与 `receive` 共享准入配置、mDNS 广播、失败任务不终止进程；`--json-lines` NDJSON 五型事件流，stdout 逐行可解析；ak-cli 83/83，真实双任务冒烟）；确定性随机性质套件（无依赖 LCG 种子可复现；25 种子随机树传输＋10 种子随机中断续传不变量，release 94.8s 零缺陷；快速回归种子 1/2/3 入默认 `cargo test`）；安卓存储通道（Kotlin MethodChannel `dev.ak/storage`：目录信息/SAF 目录选择/SAF 写闭环探针/持久授权列举，APK 179MB 重建；模拟器端选择器走查待 GUI 路线恢复）。逐项证据见 [BACKLOG](BACKLOG.zh-CN.md) 第十五轮节。AGENTS.md 同日由所有者增补并行开发六规则（见 AGENTS.md，此处不重复）。

## 实现与验证边界（如实）

- 已验证范围＝本机环回（L2 形态）＋模拟器↔宿主跨设备（CLI 路线：真实双进程、跨网络栈 NAT 形态、逐字节一致）：QUIC 握手、传输、续传、大文件、CLI 双进程。两台物理机器、跨网段、公网 NAT 穿透、抓包（L3/L4）未执行。
- 打洞：holepunch 探测与 probe 命令已实现并有本地 UDP 互探测试；真实 NAT 环境成功率未测（P0-05）。
- 移动端/Flutter 现状（第十五轮后如实）：Flutter Windows 桌面端（含配对页，真 DLL 集成测试）与安卓平台壳＋存储通道（SAF 闭环探针，构建级验证）已就位；安卓 GUI 模拟器走查受阻（System UI ANR，应用进程本身存活）；安卓 mDNS 发现、SAF 选择器实机走查、后台生命周期、真机验收、性能调优（多流并发、千兆跑满）未开始。
- 已知限制见各 crate 文档与 SECURITY：接收端 available_bytes 为真实实现（fs2）、mtime 尽力回写（filetime）、IPv6 前缀按 /64 简化、probe nonce 非密码学随机（身份由 QUIC 握手承担）、SameEgress 存在反代/CGNAT 误报场景（仅排序提示）。

## 重启恢复手册（2026-11-04，WHPX 启用后首次重启）

重启原因：Android 模拟器 x86_64 需硬件加速；本机 Hypervisor 在位，已 `Enable-WindowsOptionalFeature HypervisorPlatform`（State=Enabled, RestartNeeded=True）。重启后按序执行：

1. `cd <仓库根>`；`git status` 确认树状态（重启时列出的十四波半成品文件——ak-ffi peer 导出与 scripts/smoke_android_host.sh——已于 2026-11-04 随 `1e6a8ab`/`f82ff77` 提交收口）。
2. 验证 WHPX 生效：`emulator -accel-check`（~/android-toolchain/sdk/emulator/）应报 WHPX 可用。
3. 启模拟器：`emulator.exe -avd ak_test -no-window -gpu swiftshader_indirect -no-audio -no-boot-anim`（后台）→ `adb wait-for-device && adb shell getprop sys.boot_completed` 轮询到 1。
4. 装 APK：`~/android-toolchain/sdk/platform-tools/adb.exe install -r flutter_app/build/app/outputs/flutter-apk/app-debug.apk`。
5. 启动 app：`adb shell am start -n dev.ak.ak_sending_app/.MainActivity`；截图 `adb exec-out screencap -p > target/android_first_launch.png`（或经插件工具 /tmp/mcp_call.py——脚本重启后需重建，内容见 docs/HANDOFF 本节末注）。
6. 跨设备实测：`bash scripts/smoke_android_host.sh`（主机接收端自动化；安卓端 UI 驱动用插件 android_ui_* 工具或 adb input）。
7. 环境路径备忘：SDK=~/android-toolchain/sdk（NDK 27.2/28.2、platform-tools、emulator、AVD ak_test）；JDK=~/android-toolchain/jdk；Flutter=~/flutter-sdk；cargo=$USERPROFILE/.cargo/bin；插件工具驱动脚本（JSON-RPC over stdio，初始化→notifications/initialized→tools/call）曾存 /tmp/mcp_call.py，重启即失——按 HANDOFF 第十三轮段描述重建或直接用 adb/emulator 命令行。

## 历史记录（第一阶段，main 分支）

README 初始化提交：`4923b178860de198867f6de7b7d9cb7942200bb3`。
完整原包提交：`5ce974c279acac2bb27bd24ba4e703042dde2861`。
2026-11-02 方案评审见 [方案评审](REVIEW_2026-11-02.zh-CN.md)；D1/D5/D6 已按所有者指示经 ADR-002 采纳，D2/D3 细节仍待确认。
第一阶段验证记录见 [Rust 验证记录](../reports/rust-validation-2026-11-02.txt)、[上传验收记录](../reports/GITHUB_PUBLISH_VERIFICATION.zh-CN.md)。

## 下一工程节点

1. **首要剩余项＝两台真实物理机器（同网段）双端实测**：跨设备首证的模拟器形态已完成（Android 模拟器→Windows 宿主 CLI 路线，经 `10.0.2.2`，逐字节一致，2026-11-04 里程碑），模拟器路线已通；下一步在物理双机上跑 `ak-cli receive`/`send`（CLI 先行，随后 Flutter GUI 双端），验证真实网卡/防火墙行为；随后 `signal-server` + `probe` 在家宽/热点组合下实测公网直连成功率（P0-05，决定产品文案）。多流并行在环回仅约 1.04–1.06x（第六轮空闲机刷新：单流 93.9 vs 多流 97.9 MiB/s），是否默认启用待物理双机网卡数据再定。
2. GUI 模拟器验证恢复（受阻记录）：GUI APK 装机成功但模拟器 System UI 不稳（三轮 ANR；应用进程存活＋topResumed 确认），GUI 截图/走查未成——根因排查方向为模拟器 WHPX/镜像稳定性，非应用崩溃；恢复后补 GUI 路线跨设备走查与安卓 SAF 选择器走查。CLI 旁路（模拟器内 ak-cli 实跑＋宿主 receiver）已提供跨设备证据，不阻塞节点 1。
3. 所有者确认 D2（公网失败提示文案）与 D3（端口映射默认开关）。
4. 视实测结果冻结网络库 ADR（评审 D1 尾项）并重估路线。
5. Flutter UI 双端化（Windows 桌面端已含配对页；Android GUI 走查待节点 2）与移动端后台/存储适配（mDNS、SAF 实机走查、MediaStore）。
经第十五轮后复核：模拟器跨设备路线已通（CLI），本地可执行范围内的跨设备证据已取得；物理双机与真实网络（L3/L4）仍需外部资源。
后续开发先读取本文件、AGENTS、ROADMAP 和现有源码；保留已有历史，禁止强推。
