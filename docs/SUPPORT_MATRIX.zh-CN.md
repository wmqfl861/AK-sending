# 系统支持矩阵

更新：2026-11-06（GUI 形态口径更新到第二十八波 v1 极简 UI；第二十九波终审修复进行中）。本表对应 [BACKLOG](BACKLOG.zh-CN.md) P0-01 的"固定系统/ABI/发行版支持矩阵"部分；"可用真机"部分见文末，仍开放。类型检查层面的目标 triple 矩阵另见 [TECH_SELECTION](TECH_SELECTION.zh-CN.md)（那一级只证明 `cargo check`，不等于本表的构建/运行结论）。

状态图例：

- ✅ **已验证**＝产物构建＋真实运行＋功能路径实测；
- 🟢 **编译验证**＝产物构建＋静态符号/结构检查通过，无真实运行；
- 🟡 **部分**＝子集验证通过，全集受阻于环境；
- ⬜ **未开始**＝无任何产物；
- ➖ **排除**＝所有者明确决策排除，属范围裁剪而非未完成。

## 主表

| 平台 / 架构 | 状态 | 已验证内容 | 构建工具 | 已知限制 |
|---|---|---|---|---|
| Windows 10+ x86_64 (MSVC) | ✅ 已验证 | CLI 全功能（send/receive/pair/jobs/daemon/doctor/probe/bench/invite/registry-server/completions，旧邀请码/多码模型——见下"CLI 两种模式"；第三十二波起另含 **v1 任务码命令族 `task code-send/code-recv/join-send/join-recv`**，真平面测试 9/9＋loopback 两终端真实传输冒烟）；GUI 桌面应用＝**v1 极简 UI（第二十八波 `7458e9f` 起：连接＋设置两页、一次性任务码、零 IP/端口/指纹）**，flutter test 86/86（`7458e9f`）→107/107（wave-29 工作树）；旧五页形态的 release 构建＋首启截图＋integration_test 真实窗口 E2E 证据属 wave-28 前时点，v1 形态的 release 构建/截图复验未执行；FFI `ak_ffi.dll`（C ABI 测试＋Dart 真实 DLL 测试，dart 129/129 为 wave-29 工作树时点）；打包 zip（[package_windows.ps1](../scripts/package_windows.ps1)，纯 Windows 17 文件 / 含安卓 20 文件） | rustup `x86_64-pc-windows-msvc`（rustc 1.97.0 首验、1.99.1 复验）＋Flutter stable（windows desktop） | 最低系统版本未做扫底（仅开发机 Windows 10 build 22624 实测）；socket2 `only_v6` 的间歇 WSAENETUNREACH 坑已如实记录（第六/七轮） |
| Android API 24+ x86_64 | ✅ 已验证 | `ak-cli` 交叉编译实跑（`--version`、TOFU 落簿、peers.book/device.key 生命周期）；模拟器↔宿主**双向**真实 QUIC＋TLS 指纹钉扎传输、逐字节一致（正向经 `10.0.2.2`；反向经 adb reverse＋本地 UDP 垫片保四元组，垫片内仍是真 QUIC/UDP 报文）；APK 构建＋AVD 装机（CLI 与 GUI 两种）——GUI 自第二十八波起为 **v1 极简 UI**（连接＋设置两页） | NDK 28.2.13676358（clang/llvm-ar，API 24）＋[build_android_ffi.sh](../scripts/build_android_ffi.sh)＋[smoke_android_host.sh](../scripts/smoke_android_host.sh)＋adb | 模拟器形态（NAT 环回），非物理设备；反向跨设备的网络垫片属测试形态如实声明；**v1 极简 UI 的模拟器/真机走查未执行**（System UI ANR 史，跨设备证据由 CLI 路线提供）；v1 任务码链路的 Android 身份目录接线属 wave-29 P1-01 范畴（进行中收口）；API 24 最低版本扫底未做 |
| Android API 24+ arm64-v8a | 🟢 编译验证 | `libak_ffi.so` 构建＋NDK `llvm-nm -D --defined-only` 冻结符号抽查（`ak_engine_new`/`ak_identity_info`/`ak_jobs_list`，二十三波后含 18 个 v2 契约符号，二十八波起另含 `ak_task_*` 任务码族）＋APK 双 ABI 随包（`7458e9f` 重建，两 ABI so 约 8.6MB）＋jniLibs 与 target 产物 SHA-256 一致 | 同上（aarch64-linux-android） | 无 arm64 设备/模拟器——符号在场不等价真机运行（[ANDROID_BUILD](ANDROID_BUILD.zh-CN.md) 标记约定） |
| Linux x86_64 (gnu) | ✅ CLI 已实测 / 🟡 GUI 未构建 | **CLI 路线已部署实测**（2026-11-05）：Ubuntu 24.04 服务器原生构建（ak-cli+ak-ffi release），Windows↔Linux 邀请码配对双向 QUIC 传输 SHA256 一致（cross_device_test.sh 6/6；2026-11-06 v2 升级后旧客户端向后兼容 6/6 复验）；宿主交叉 check（十 crate 全集）仍受阻于无 `x86_64-linux-gnu-gcc`（环境非代码，零 `error[E...]`）；Flutter Linux runner 已生成（[flutter_app/linux/BUILD.md](../flutter_app/linux/BUILD.md)：CMake、GTK runner、`libak_ffi.so` 部署与 RPATH 配置齐备），**Linux 主机上从未构建过 GUI**（目标形态＝v1 极简 UI） | 需要 Linux 主机：Flutter stable（linux desktop）＋clang/cmake/ninja-build＋GTK3（pkg-config）＋Rust（`libak_ffi.so`）；交叉仅能到 check（[check_targets.sh](../scripts/check_targets.sh)、[check_linux_targets.sh](../scripts/check_linux_targets.sh)） | 从未在 Linux 主机执行过构建或运行；CI `portability` job 属计划（从未在 Actions runner 实跑，不得引用为已验证结论）；v1 极简 UI 在 Linux 的验证随首次 GUI 构建一并开放 |
| macOS (aarch64/x86_64) | ➖ 排除 | 无（两 apple triple 的十 crate check 同样受阻于宿主无 Apple SDK，属环境；排除决策与此无关） | — | 所有者明确排除（2026-11-03 P1-04 范围裁剪，BACKLOG 注记在案）；即使工具链具备也不在目标内 |
| iOS | ➖ 排除 | 无 | — | 所有者明确排除（同上；P0-06 的 iOS 部分随之裁剪） |

## 各平台详注

### CLI 两种模式（第三十二波口径更新）

`ak-cli` 与 GUI 并非同一交互模型的两个入口，如实区分：

- **模式一（旧邀请码/配对簿多码模型，CLI 全功能保留）**：`pair`/`mDNS` 配对簿、`invite create/redeem/list/revoke`、`daemon --pairing` 两维并发（第二十七波 `5a5dac1` 起真实链路）、`registry-server`——适用于自动化、运维与高级场景；Windows/Linux（部署实测）与 Android x86_64（实跑）均验证。
- **模式二（v1 一次性任务码模型，第三十二波起 CLI 与 GUI 双入口）**：第二十八波（`7458e9f`）新增的 `ak_task_*` FFI→Dart→Flutter 链路；第三十二波新增 **CLI 任务码命令族 `ak-cli task`**（`code-send`/`code-recv`/`join-send`/`join-recv`，构建于 ak-session `TaskPairing`+ak-engine+ak-net 公开 API，不依赖 ak-ffi）——生成方打印醒目 AKI1 码块、接收方 30 秒 [Y/n] 同意提示（超时拒绝、零落盘）、双向真实探测、指纹绑定接收准入、断开撤码+尽力通知对端、对端断开以明确原因非零退出；ak-cli 内嵌 9 项真平面测试（双方向 e2e、拒绝/超时分支、方向冲突门禁不烧码、对端断开）+ 本机 loopback 两终端真实传输冒烟（字节一致）。此前"v1 任务码 GUI-only（CLI 未接）"的限制就此关闭；服务器侧 v2 schema 已部署（122.51.121.24），旧客户端向后兼容 6/6。

### Windows 10+ x64

主开发与验证平台。全部 28 个开发波次在此执行：workspace 测试全绿（第二十三波后 918/0 → 第二十八波 `7458e9f` 时点 1085/0；wave-29 修复后全量待复跑）、clippy `-D warnings` 干净、5 GiB release 回归、20k/65,537 海量文件、GUI release 构建与真实窗口 E2E（旧五页形态，wave-28 前时点）、FFI DLL 的 C ABI 与 Dart 双侧测试、打包脚本两次自测幂等。发行形态：`ak-cli.exe`＋`ak_ffi.dll`＋GUI exe（[PUBLISHING](PUBLISHING.zh-CN.md)）。

### Android API 24+（x86_64 运行验证 / arm64 编译验证）

minSdk 24 与 NDK clang 的 `android24` 目标三元组对齐（第十三轮）。两 ABI 的 `libak_ffi.so` 均以 `build_android_ffi.sh` 构建，符号经 NDK `llvm-nm` 抽查冻结契约（第二十八波起含 `ak_task_*` 任务码族，`7458e9f` 重建双 ABI so）。运行验证只在 x86_64 模拟器完成（含双向跨设备传输）；arm64 产物未上过任何设备。APK 为 debug 形态（未做 size/strip 优化）；GUI 自第二十八波（`7458e9f`）起为 **v1 极简 UI**（连接＋设置两页），其模拟器/真机走查未执行；SAF 存储通道代码已落地但选择器走查未执行（P0-06 余项）。

### Linux x86_64

代码侧无已知移植障碍（十 crate 在 linux-gnu triple 零编译错误；阻塞全部来自第三方 C 构建脚本找目标编译器）。Flutter Linux runner 与构建说明已就位（目标形态＝v1 极简 UI，第二十八波起），构建动作需 Linux 主机上的 clang/cmake/ninja/GTK3——本仓库尚未执行过。

## 工具链版本记录

| 工具 | 版本 | 首次验证 |
|---|---|---|
| rustc（Windows MSVC） | 1.97.0（首验）→ 1.99.1（复验） | 2026-11-02 / 11-04 |
| Android NDK | 28.2.13676358（API 24） | 2026-11-04 |
| Flutter stable | windows desktop 构建实测；linux desktop 仅 runner 生成 | 2026-11-03 / 11-05 |
| Dart SDK（随克隆） | ak_ffi_dart 107/107、analyze 零告警 | 2026-11-03 |

## 可用真机清单（P0-01 仍开放部分）

- Windows 物理机 1 台（开发机，Windows 10 build 22624）——充当跨设备传输的宿主端；
- Android 模拟器 AVD（x86_64，API 级见 [ANDROID_BUILD](ANDROID_BUILD.zh-CN.md)）——**非物理设备**；
- Linux 云主机 1 台（Ubuntu 24.04，122.51.121.24，部署+CLI 实测用）；物理双机（两台真实机器跨网段）、arm64 真机、Linux 桌面主机/macOS/iOS 主机：均无。

因此 P0-05（受限 NAT/跨运营商样本）、P0-07（真机抓包）等依赖外部网络与物理设备的项保持开放，见 [BACKLOG](BACKLOG.zh-CN.md)。
