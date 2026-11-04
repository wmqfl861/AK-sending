# 技术选型与验证门槛

## 状态更新（2026-11-02）

网络路线已按 [ADR-002](adr/002-protocol-stack-and-order.zh-CN.md)（已接受）收敛，不再处于"未冻结"状态：

- 数据面＝QUIC（quinn）＋rustls（ring 后端）＋自签 Ed25519 证书＋指纹钉扎；控制面＝自建信令（TCP、定长有界消息）＋内置最小 STUN（RFC 5389 子集）。iroh 已排除（打洞依赖 relay，与 ADR-001 冲突），下表中的候选对比保留作为决策记录。
- 依赖已从"仅标准库"放宽为受控清单（tokio、quinn、rustls、ring 系、ed25519-dalek、blake3、rcgen、mdns-sd、if-addrs、clap 等，均锁定于 Cargo.lock），仓库检查脚本持续做禁依赖扫描（relay/TURN/WebRTC/iroh，2026-11-02 结果 clean）。
- 验证现状：环回 L2 原型验证已执行（workspace 测试、clippy、5 GiB 稀疏文件 release 测试、双进程 probe 全链路，见 [dev-validation-2026-11-02](../reports/dev-validation-2026-11-02.txt)）；跨网段/真实 NAT、真机与服务器端抓包（L3/L4）未执行。下文"冻结门槛"中的各项仍为计划。

## 已选方向与未冻结部分

Flutter 作为五端 UI 候选，Rust 共享核心、SQLite 本地持久化、Swift/Kotlin 原生适配。
UI 官方平台支持不等于第三方插件全覆盖，更不等于本项目已经通过五端构建。[S5]
本次 Rust 核心仅使用标准库，无网络依赖、无数据库驱动、无 Flutter 绑定。

| 网络路线 | 优点 | P0 必须解决的问题 |
|---|---|---|
| iroh/QUIC 严格禁用 relay | 有成熟端点/加密与连接组件 | relay 参与打洞协调；禁用后独立协调可行性、路径证据、五端接口 |
| QUIC 库＋独立信令/穿透层 | 数据通道责任明确 | 需要额外实现与测试穿透、候选调度和移动恢复 |
| WebRTC DataChannel，无 TURN | 可复用 ICE 候选机制 | 禁用 relay 候选、消息背压、块大小、五端绑定和独立传输性能 |
| C++＋libdatachannel，无 TURN | C++ 团队可采用 | 非当前已确认生产实现；需独立评估维护与平台支持 |

不要把“配置 RelayMode::Disabled”写成“保证打洞仍完整”。官方 Builder 文档说明 relay 协助建立连接和打洞。[S3][S4]
P0 须验证客户端根本不会连接公共 relay，且所用协调服务只传有限信令；不能允许先经 relay 传握手后的文件再升级直连。
STUN 仅是穿透工具，不独立保证端点间可达。[S1]

## 冻结门槛

五端构建；Windows/Android/iOS 真机最小双向收发；完全无外网 LAN；跨运营商 IPv4/IPv6；禁 UDP 环境；双端受限 NAT 的成功/失败记录；服务器端抓包。
验证路径切换、移动后台中断、目录句柄随机写能力、绑定维护情况、许可证和发布签名。
所有结果保留具体设备/系统/网络/库版本和错误，不编造统一穿透成功率。

## 依赖管理

冻结后锁定确切版本/特性与 lockfile，更新时重新跑无中继矩阵。
不使用“latest”作为可复现构建版本，不自研加密算法，不以特性开关隐藏生产中继选项。
公共协调设施仅用于经评估的实验，不把未知第三方服务当产品生产依赖。

## 目标平台矩阵（2026-11-04 本地 check）

目的：用 `cargo check`（类型检查，不链接、不运行）证明纯 Rust 核心（ak-core / ak-wire / ak-identity / ak-storage / ak-store / ak-engine / ak-signal / ak-net / ak-cli / ak-ffi 十个 crate，ak-store 默认文件后端）在非宿主 triple 上的可移植面。脚本：[scripts/check_targets.sh](../scripts/check_targets.sh)（Android 无 NDK 时打印原因跳过）；CI 镜像：[.github/workflows/ci.yml](../.github/workflows/ci.yml) 的 `portability` job。

本地实跑（已执行）：Windows 10 22624 + Git Bash，rustc 1.99.1 stable-x86_64-pc-windows-msvc，NDK 28.2.13676358（API 24）。结果：

| 目标 triple | 十 crate 全集 check | 首错（截断） | 分类 |
|---|---|---|---|
| x86_64-linux-android | 通过 | — | 已由 Android release 构建另行验证（[ANDROID_BUILD](ANDROID_BUILD.zh-CN.md)） |
| aarch64-linux-android | 通过 | — | 同法（真机 arm64） |
| x86_64-unknown-linux-gnu | 受阻（环境） | `failed to run custom build command for ring v0.17.14`（cc-rs 找不到 `x86_64-linux-gnu-gcc`） | 宿主缺交叉 gcc，非代码问题 |
| aarch64-apple-darwin | 受阻（环境） | `failed to run custom build command for blake3 v1.8.7` / `ring v0.17.14`（cc-rs 找不到 `cc`） | 宿主缺 Apple SDK（`TargetConditionals.h`），非代码问题 |
| x86_64-apple-darwin | 受阻（环境） | 同 aarch64-apple-darwin | 同上 |

受阻原因的定位证据（区分“代码问题”与“纯目标库/工具链缺失”）：三份受阻日志中零 `error[E...]`、零指向 `crates/ak-*` 的编译错误；失败全部来自第三方 C 构建脚本（ring 0.17.14、blake3 1.8.7 经 cc-rs 找目标 C 编译器）。已用 NDK clang 探测证实宿主连 Apple SDK 头与 glibc 头都不具备。补强：无 C 构建脚本依赖的纯 Rust 子集 {ak-core, ak-wire} 在全部 5 个 triple 上 check 全部通过。结论：ak-* 自身代码未发现平台特定错误（含 cfg 误用）；linux-gnu 与两 apple 目标的十 crate 全集原生验证由 CI `portability` job（ubuntu-latest + macos-13）承担。

已知限制：本矩阵只证明类型检查层面；不等于链接、构建或真机运行。iOS 真机、apple/linux 原生链接与运行时行为均未验证。CI `portability` job 属计划（仓库未推送到 GitHub，从未在 Actions runner 上实跑），不得引用为已验证结论。
