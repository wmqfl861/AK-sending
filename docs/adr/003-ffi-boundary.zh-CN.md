# ADR-003：FFI 边界采用句柄式 C ABI

- 状态：已接受（2026-11-03，Rust 侧契约冻结；同日实现落地并经 C ABI 端到端测试。2026-11-04 第十七波修复带来契约增补——见下文"修复波落地中的契约增补"节，各项落地前以 `crates/ak-ffi` 当前实现为准，落地证据由集成者补）
- 关联：ADR-002（"接口契约先冻结再并行开发"的推进模式、模块拆分）；[评审 4.5](../REVIEW_2026-11-02.zh-CN.md)（Flutter + Rust 路线与推进顺序第三步）；[SECURITY"FFI 边界"节](../SECURITY.zh-CN.md)（边界防护与已知限制登记）

## 背景

评审 4.5 确认 UI 路线为 Flutter + Rust 核心：LocalSend 已用 Flutter 覆盖五端，证明该路线可行；Tauri 2 桌面包体更小，但移动端原生能力（SAF、后台任务、Bonjour）生态较弱，对本项目不划算。推进顺序中 Flutter 界面是第三步，前两步（局域网 CLI 原型、公网直连探测）的 Rust 侧核心已就位，UI 需要一个绑定层才能消费 `ak-engine`。

本文写作时（2026-11-03）Flutter SDK 尚未安装、UI 工程尚未启动，按 ADR-002 确立的"接口契约先冻结再并行开发"模式，Rust 侧先把对外契约冻结为 C ABI，FFI 实现与后续 Flutter 工程并行推进、互不阻塞。该模式已按计划走完：`ak-ffi` 实现于第十轮落地（9 个导出函数，全部经 C ABI 测试）；Flutter Windows 端已于第十一至十三轮发布——`flutter_app` 五页 GUI（`flutter build windows --release` 产物 43.9 MB 打包、真实窗口 integration test 全绿）＋`packages/ak_ffi_dart` 绑定（85 项 Dart 测试，含真实 DLL 环回）；Android 壳已构建实测（APK 内 x86_64/arm64 双 ABI `libak_ffi.so`），`ak-cli` 交叉编译至 Android 并在模拟器实跑完成跨设备真实 QUIC 传输（取证见 [dev-validation-2026-11-02](../../reports/archive/dev-validation-2026-11-02.txt) 第十一至十六轮节）。同时，FFI 是 `unsafe` 的天然来源，也是内存安全审计面从"Rust 内部"扩大到"任意语言调用方"的位置，边界形状需要一次性收窄、集中可审。

## 决策

`crates/ak-ffi` 对外暴露句柄式 C ABI（契约草案全文见 `crates/ak-ffi/src/lib.rs` 头注释，由 FFI 实现代理冻结）：

1. **句柄式、无全局状态**：`ak_engine_new` 创建引擎实例并拥有一个内嵌 tokio 运行时，其余每个调用都以该句柄为第一参数；不导出全局单例，多实例可并存。调用方（Dart 或其他语言）无需理解 async 运行时的存在。
2. **轮询式进度 JSON**：进度以小 JSON 行经轮询（pull）取回，任何语言的事件循环都能渲染；错误为状态码＋消息缓冲区。不使用回调函数指针——回调会把调用约定与线程语义强加给调用方，且是 unsafe 的另一个来源。
3. **调用方缓冲区出参**：字符串进入侧为 UTF-8 `*const c_char`，返回侧写入调用方提供并声明容量的缓冲区；除显式释放 API 外，不把 Rust 侧分配交给调用方释放，规避跨分配器 free。
4. **spec 走 JSON 且未知字段忽略**：任务描述（spec）以 JSON 传入，解析忽略未知字段——UI 与核心可独立演进，旧调用方遇到新增字段不失败（向前兼容），不因版本同步问题阻塞任何一侧。
5. **`unsafe` 仅限本 crate 的 shim，逐块 SAFETY 注释**：所有业务逻辑位于 shim 之下的安全函数中。这是全仓库 `#![forbid(unsafe_code)]` 的唯一例外（其余 crate 全部 forbid），理由：FFI 边界天然必须解引用外部指针，无法 forbid；将 unsafe 压缩到一个不含业务逻辑的薄层并逐块注释 SAFETY，是最小的、可人工整读的审计面，例外范围不随功能增加而扩大。

## 修复波落地中的契约增补（2026-11-04，第十七波）

2026-11-04 Codex 只读审核（[codex-review-2026-11-04](../../reports/archive/codex-review-2026-11-04.md)）立项第十七波修复，6 个修复代理并行改 crate，其中以下变化触及本 FFI 契约。各项均为**修复波落地中**——落地前以 `crates/ak-ffi` 当前实现为准，落地后由集成者补执行证据并回填本节与 SECURITY"FFI 边界"：

- **`expect` 必填**（准入收窄）：receive spec 的 `expect` 字段由"可选、缺省 `any`"改为必填——来者不拒不再是隐式默认，调用方必须显式声明准入策略（`any`/`book`/64 位指纹/6 位配对码）；缺省或未知值以 `AK_ERR_INVALID` 拒绝。
- **摘要算法未知显式拒绝**（F1）：spec 与清单携带的摘要算法标识必须为已知枚举值（当前仅 BLAKE3），未知值显式报错，不再静默按 BLAKE3 处理（审核第 7 条：原实现经 `ak-core::FileEntry::new` 丢字段写死 BLAKE3）。
- **任务/检查点身份分离**（F2）：FFI job id、wire job id（内容身份）与检查点 record id（纳入方向/对端/目标身份）三者语义分离；进度/结果 JSON 字段与持久记录命名相应调整，同源并发多对端不再共享覆盖同一条记录（审核第 4 条）。
- **冲突原子 no-replace**（F3）：`conflict: "fail"` 的"绝不静默覆盖"由提交前检查升级为平台原子 no-replace 保证（Windows 原子语义＋POSIX link 方案），FFI 冲突策略语义不变、保证强度提升。
- **禁用 QUIC active migration**（F5）：存活连接不再静默迁移对端地址，建立时的准入/钉扎/路径闸门判定不会因迁移被绕过；迁移重授权机制为后续项。
- **spec 边界校验随引擎收窄**（F6）：块尺寸/空闲预算等字段与引擎侧 Offer `chunk_size` 边界防御（≤max_chunk_bytes，审核第 1 条 P1）联动，超范围值显式 `AK_ERR_INVALID`，空文件不申请块缓冲。

## 备选方案与取舍

- **flutter_rust_bridge 代码生成**：类型安全、免手写 shim、Dart 侧体验最好，但要求 Flutter/Dart 工具链先行（SDK 未装即阻塞，与"先冻结契约并行开发"冲突）；生成代码把 unsafe 面摊开到难以逐块审计的位置；Rust API 形状与 Dart 强耦合，语言无关性丧失。选定方案的 C ABI 语义足够小，未来切换时边界语义可平移（见后果）。
- **JSON-RPC 子进程**：进程隔离最干净（核心崩溃不连带 UI 进程），但五端需打包两个二进制、多一层进程管理与逐调用序列化开销，进度轮询延迟受 IPC 通道限制；对单用户本地工具过重。可作为未来崩溃隔离的备选形态保留，不作当前绑定层。
- **纯 CLI 管道（stdout JSON Lines）**：零 FFI、直接复用 `ak-cli`，但管道是单向流，表达不了句柄式双向控制（创建后多次查询/暂停/取消共享同一任务状态），且每次调用冷启动；保留为脚本与诊断用途，不作 UI 承载。

## 后果

- **UI 侧语言无关**：任何能调用 C ABI 的语言（Dart FFI、Swift、Kotlin JNI 等）都能绑定，不锁定 Flutter；Flutter 侧绑定层可独立演进。
- **内存安全审计面集中**：unsafe 全部位于 `ak-ffi` shim 一层，逐块 SAFETY 注释可人工整读；边界防护设计（指针入参非空/UTF-8 校验、缓冲区容量协议、控制字符拒绝、句柄生命周期）在 SECURITY 文档"FFI 边界"节登记。
- **可平移性**：若未来换用 flutter_rust_bridge，句柄生命周期、JSON spec、轮询进度的边界语义可原样平移，迁移成本限于重写 shim 一层。
- **代价**：缓冲区容量协议与句柄生命周期约定把误用面转移到调用方（容量虚报、截断、竞态、free 后使用），如实登记于 SECURITY 已知限制；轮询频率与 UI 帧率的权衡由调用方决定。
- **验证形态（已随实现落地）**：Rust 侧测试直接驱动 C ABI 端到端（经边界的真实回环传输、坏 spec 反例、取消/并发/双引擎无串扰）自第十轮起锁定；Flutter Windows 已接入真实 DLL（三级探测加载）并有真实窗口 integration test；Android 壳＋CLI 已实测。2026-11-03 前后"尚未接入任何前端"的表述已过时。第十七波契约增补（上文）落地后需重跑 C ABI 回归并回填本 ADR 与 SECURITY。
