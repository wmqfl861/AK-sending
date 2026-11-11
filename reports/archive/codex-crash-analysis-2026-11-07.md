**AK-sending Windows GUI 间歇性原生崩溃诊断与修复建议 · 2026-11-07**

本次结论：现有证据已经把问题收敛到“成功加载并使用 native 引擎后，Flutter Windows release 宿主内出现原生故障”，但还不能确定首次破坏内存的位置，也不能认定最后一次 `ak_pairing_poll_event` 是根因。优先排查进程内分配器/FFI 边界，以及 Flutter Windows engine、渲染和线程初始化与 DLL 共处时的交互。与此同时，必须先控制实际加载产物：现有打包和探测代码确实允许诊断时混用旧 DLL、新 DLL、桩 DLL 和不同 AOT 产物。

**未发现当前正常启动调用链中可以直接证明的 FFI 输出越界写或缓冲区提前释放。** 找到两个条件明确的原生安全问题：runner 的 `WM_FONTCHANGE` 分支可空指针解引用；Rust 对外提供的 safe 函数接受任意裸指针并可触发 UB，作为 `rlib` API 不健全。它们都不能直接被宣布为这次首启崩溃的根因。另有分配器来源不固定、无界 C 字符串读取、借用句柄缺少内建存活检查等防御缺口。

如果近期目标是保证 native 引擎故障时 GUI 仍可使用，应把**全部 DLL 加载和引擎执行移入本机独立 worker 进程**。延迟加载、Dart isolate、`try/catch`、加载重试都不能在同一进程内隔离访问违例或 fail-fast。

本报告按以下证据边界阅读：

| 标记 | 本次实际依据 |
|---|---|
| 用户实测 | 用户提供的 7 组事实，包括退出码、6 份转储摘要、隔离矩阵、DLL 替换干扰、CLI/Android/debug 表现；原样作为诊断前提，没有代替用户重新实验 |
| 源码事实 | 当前 HEAD `0cf138297f4919649083b65162f752d2e97faf22`，开始时工作树干净；阅读 README、HANDOFF、ROADMAP、ADR-001、既有评审及相关代码；Rust FFI、Dart 包、Flutter/runner 分别并行审查，主评审交叉核对 |
| 本地 SDK 线索 | 阅读了已有 `flutter_app/windows/flutter/ephemeral` wrapper/header；这些是本地生成文件，不能单独证明现场 `flutter_windows.dll` 与之匹配 |
| 尚待执行 | 报告内所有实验、调试器命令、改码方案和效果验证。没有运行构建、测试、Python 仓库检查、clippy、Dart/Flutter analyze、GUI/CLI、转储调试或网络实验 |

唯一新增文件是本报告。未修改源代码、构建配置、依赖或系统设置，未打开用户转储、身份密钥或传输文件。没有把既有文档中的“通过”当成本次验证结果；README/HANDOFF/ROADMAP 仍有 wave-29 进行中的旧状态，判断以当前代码为准。所有行号对应本次源码快照；本报告中的 `C:\akdiag\...` 是建议使用的示例目录，不是真实用户文件位置。

**先正确解释现有异常证据。**

`0xC0000409` 的符号名常显示为 `STATUS_STACK_BUFFER_OVERRUN`，但现代 Windows 用它承载多种 fast-fail 原因，不能仅凭该名称就认定栈缓冲区被覆盖。需要看异常记录的 fast-fail 子码。例如 stack cookie、链表损坏、CFG 间接调用检查失败、主动 fatal exit 的判断完全不同。

转储摘要中的 `0xC0000005` 是访问违例。它与进程退出码 `0xC0000409` 可能对应“先 AV、后终止”的不同阶段，也可能来自不同轮次或不同捕获方式；**目前不能假定两者已建立同一 PID、同一时间、同一异常链的对应关系**。LocalDumps 的数量和扩展名也不能替代对异常记录和 dump 类型的核对。

| 现象 | 可以推出 | 不能推出 |
|---|---|---|
| 异常地址是高位 x64 地址 | 应查该 dump 的已加载模块表与内存区属性 | 不能仅据地址外观认定属于系统 DLL；应用 DLL、Flutter engine 也会映射到高位 |
| `RIP == ExceptionAddress` | 对 AV 通常是正常的异常现场描述 | 不能据此认定跳到了坏函数指针；必须读访问类型和被访问地址 |
| 栈上反复出现类似堆地址的数值 | 需要可靠展开、寄存器、对象/堆元数据 | 不能据裸栈数值重复就宣布栈损坏；参数、局部对象、保留值也会重复，地址归属须查询 |
| 最后日志是 poll | 该导出至少记录过入口，若日志与 PID 对应正确 | 不能证明已经进入函数主体、已经返回，或崩溃线程就是该调用线程 |
| 某轮没有 Rust 日志 | 没有收到这套日志机制的记录 | 不能证明 DLL 没被加载或故障在 `DllMain`；可能是另一版本、未走成功分支、日志打开/写入失败或未及时落盘 |
| 存活但模块列表没有 `ak_ffi.dll` | 该轮需要单列加载状态 | 不应计入“真实引擎正常运行”的成功样本；还要排除检查错 PID、加载尚未发生或模块枚举失败 |
| CLI 数千次稳定 | 强烈削弱共享 Rust 业务逻辑的普遍、确定性错误 | CLI 没有覆盖 Dart allocator、FFI trampoline、Windows DLL 加载和 GUI-only v1 task 平面 |

对 AV，异常参数通常是 `ExceptionInformation[0] = 0/1/8`，分别表示读/写/执行，`[1]` 才是目标地址。若指令位于 `ntdll!RtlFreeHeap`，可能是此前别处破坏了堆；若是执行权限错误，要看目标内存属性、函数地址和映像生命周期；若是近零地址读，则应优先检查空对象。**故障指令所在模块与最初造成错误的模块不是同一概念。**

**当前源码的真实启动顺序。**

用户提到“`task_start` 只校验 spec”以及“Tokio 在 poll 中惰性创建”的可能性，与本次读取的源码有区别。当前 `task.rs` 已超过 6200 行，且未检索到用户临时加入的 `flog`。以下描述针对此快照，现场各 DLL 是否等价仍需核对。

```text
Windows runner 创建 Flutter engine / 启动 Dart main
  main.dart:41–47
    bootstrapAppService()
      DLL 路径探测
      owner.borrowEngine()
        facade 再探测路径 → DynamicLibrary.open → 绑定导出
        ak_engine_new
          立即创建 2-worker Tokio runtime + I/O/time driver
      missingTaskExports（此时 engine_new 已执行）
      构建 task bindings 与 service；保存 owner.dispose 闭包
    runApp
      AppShell.initState → service.start
        ak_task_start
          校验配置 → 创建目录 → 加载/创建身份密钥
          指纹计算、随机 runtime ID → 本地表注册 → runtime_ready 入队
        注册 50ms Timer
        同步 _pumpOnce
          poll_event 取 runtime_ready
          poll_event 取空
          poll_progress 取空
        task_list 获取本地快照
      后续 Timer tick 重复轮询

用户生成/输入任务码后，才进入 ensure_stack → registry/配对网络初始化。
```

关键定位：

| 阶段 | 源码证据 | 诊断意义 |
|---|---|---|
| 首帧之前创建 native 引擎 | `flutter_app/lib/main.dart:41`、`:87`；`services/ak_engine_facade.dart:173`；`packages/ak_ffi_dart/lib/src/engine.dart:93` | 缺 DLL 的容错分支跳过了整个成功路径，不能仅以该矩阵区分加载、分配、线程和业务调用 |
| Tokio 立即创建 | `crates/ak-ffi/src/core.rs:918`–936；`shim.rs:185`–195 | `Builder::new_multi_thread().worker_threads(2).enable_all().build()` 在 engine_new 内发生；poll 不是首次建 runtime |
| task_start 有本地副作用 | `task.rs:1053`、`:1061`、`:1065`、`:1071`、`:1072`、`:1109` | 包括目录、身份、密码学/RNG；`runtime_ready` 仅表示本地 task context 就绪，不表示 registry 已连接 |
| 网络延迟到操作时 | `task.rs:1208`、`:1234`；调用点 `:2524`、`:2933` | 无用户操作的启动崩溃中，registry/TLS/QUIC 初始化优先级应降低 |
| poll 仅查表、出队、拷贝 | `shim.rs:1151`–1205；`pairing.rs:916`；`task.rs:3375` | 空桩改变的不只有逻辑，还包括内存分配、锁、输出解析和时序 |
| task_list 只做本地快照 | `task.rs:2170`–2197 | 不建立连接；路径长度可能改变 JSON 大小并触发正常 `-3` 扩容 |
| Dart 首次同步泵与 Timer | `services/simple_transfer_service.dart:343`–358、`:945`–965 | 同一 isolate 的同步 FFI 不会被 Timer 回调打断；Rust worker 仍可并发 |

因此用户某次日志 `engine_new → task_start → poll_event ×2 → poll_progress → task_list → poll_event` 与当前设计的正常启动顺序吻合。它既没有证明第二次 poll 的生命周期错误，也没有排除另一条原生线程此时发生故障。

**暂定根因排序。** 这是根据当前证据作出的可能性排序，不是已测得的概率；前三项证据接近。工件控制虽然排第三，却必须最先做，因为它决定其他实验是否可解释。H1/H3/H5/H6 可以共同表现为堆损坏或错误调用，不能机械相加成多个已发现根因。

| 排名 | 假设与当前权重 | 匹配证据 | 矛盾点、目前缺口 | 首个低成本验证 |
|---|---|---|---|---|
| **H1** | **进程内 native 内存破坏，首查分配器来源与 FFI 边界；中等** | AV/可能的 fail-fast；Flutter Windows 宿主特异；poll 高频 allocate/copy/free；`malloc/free` 独立全局解析是具体风险点 | 当前正确 ABI/缓冲纪律未发现必然越界；还不知道 malloc/free 实际来源；若真正 load-only 即崩，此分支大幅降级 | E0 异常分类；E2 查询两个函数地址所属模块；E4 分阶段；必要时 E5 PageHeap |
| **H2** | **Flutter Windows AOT/engine/渲染或窗口生命周期缺陷，被 DLL 加载、线程和分配时序触发；中等** | release 特异、RDP、Impeller 路径；加载 DLL 可改变首帧时序/地址布局；现有 Impeller 开关未证明生效 | 缺 DLL 时也运行 Flutter 且稳定，削弱无条件 Flutter 崩溃；没有符号栈指向 renderer/VM；AOT 不等于有 bug | E0 看 faulting thread；E6 确保 debug/profile/release 真走同一 native 路径；E7 有效后端与线程策略对照 |
| **H3** | **实际构建产物/ABI 混用、加载来源不一致；中等，且是确定存在的诊断混杂因素** | 反复换真/桩 DLL；部分存活轮无模块；CMake 只 POST_BUILD 复制；打包可能带两份不同 DLL；符号探测不验证 ABI | 不能单独解释“同一完整 bundle、同一映射版本已用哈希确认”后的随机故障；用户的同二进制间歇性事实仍须保留 | E1 冻结完整 bundle、记录实际 loaded image；E4 用同一 DLL 改宿主阶段，禁止逐轮换 DLL |
| **H4** | **engine_new 的 Tokio/IOCP/CRT/TLS 初始化，或 task_start 的本地依赖存在 Windows 宿主交互；中低** | 真 DLL 成功路径才建 worker/访问身份；线程/系统依赖可有延迟故障；CLI 的宿主模型不同 | 创建独立 Tokio runtime 是受支持用法；未见业务 DllMain/loader-lock 初始化；poll 不建 runtime；共享代码 CLI 很稳定 | E4 停在 open/engine_new/task_start 后空闲；E8 原生 DLL 小宿主与 Dart AOT 小宿主对照 |
| **H5** | **engine owner 提前回收、dispose/finalizer 后裸指针使用，或错误卸载；低至中低** | 普通 Dart Finalizer 会 free；task bindings 只持裸指针；AOT/GC 时机会放大错误生命周期 | 当前生产强引用链完整，首启无 dispose；已修复旧接收等待 UAF；没有 DLL 卸载代码证据 | E3 对 engine_free 下断点，分清显式 dispose/Finalizer；记录调用 generation；诊断性保活对照 |
| **H6** | **加载期依赖、映像/重定位、CFG 或错误函数目标；低，若 load-only 复现则上调** | 无导出日志轮次、DLL 替换关联；执行型 AV/特定 fast-fail 子码可支持 | 无日志不能证明加载期；普通 ASLR/CFG 兼容行为不是错误；未有模块+RVA、PE load config 或子码 | E0/E4；查看依赖、映射地址、PE relocation/load config；定位首次异常而非先关安全特性 |
| **H7** | **ICU/路径/cwd/身份目录触发；低** | 启动有路径探测与身份 I/O；相对 AK_FFI_DLL 或开发回退会受 cwd 影响；路径长度会改变缓冲压力 | exe 旁 DLL 优先，切 cwd 不能稳定改变结果；缺 DLL 时 Flutter 正常渲染；本地 Flutter header 明确 data 路径相对 exe | E9 固定绝对 DLL 路径、完整资源包、可丢弃身份/保存目录，分开控制非 ASCII/长度 |
| **H8** | **SmartScreen/MOTW/文件缓存状态本身导致崩溃；很低** | 重拷贝可能改变元数据、文件身份和缓存；SmartScreen 与 Defender 不是同一机制 | 这些通常表现为启动拦截/加载失败/时序变化，不直接解释已进入导出的 heap AV；没有相关事件证据 | 先完成 E1，再比较同字节文件的元数据/模块基址；只在出现明确系统事件时追查 |

关于 H1 的具体性：`bindings.dart:79`、`:83` 各自通过 `DynamicLibrary.process()` 惰性查找 `malloc` 与 `free`，没有绑定同一个 CRT 模块。源码注释“没有跨 allocator free”只证明 Rust 不释放 Dart 缓冲，未证明这两个全局符号必定配套。Windows 宿主可能同时有多套 CRT/导出；但在模块集合与顺序稳定时，两者通常仍会落到配套实现。**不能因看到这段代码就宣称已经证实跨堆释放。** 应检查实际指针和最终供应模块；导出转发需追到最终地址。两个模块名不同也不必然表示不同堆实现，要核对转发和实现。

关于 H2/H4：Flutter 的 UI isolate、platform thread、raster thread 和 Tokio worker 是不同角色。当前没有 `NativeCallable`、Rust 回调 Dart、`isLeaf: true` 或跨 isolate 共享 pointer 的证据，因此“Rust 在错误线程调用 Dart”不是现成解释。后台线程存在，也不代表同步 timer 会重入。需用线程 ID 和可靠栈判定是哪条线程出错。

关于 H5：可从当前源码追出 `AkSendingApp.service → AppShell.service → FfiSimpleTransferService.engineOwner → owner.dispose 方法闭包 → facade._engine` 的保活链。不能仅凭 AOT 或缺少 `implements Finalizable` 就宣布 owner 被提前 GC。直接裸指针借用 API 仍值得改进，但是否发生 free 是可直接观察、应先观察的事实。

**“DLL 刚替换后比较稳定”的解释，按证据强弱处理。**

| 机制 | 合理解释范围 | 不能当作结论的部分 |
|---|---|---|
| 实际机器码/依赖变了 | 8.5 MB、7 MB、空桩等会改变代码布局、堆压力、初始化、导出和时序；它们不是等价 A/B | “桩版 4/4”不能独立证明被删的逻辑是根因 |
| 打包/复制与加载副本不同 | exe 旁副本、Cargo 输出、显式 env、CMake cache、打包子目录可能不同；当前有代码依据 | 文件名相同或磁盘上刚拷贝，不等于运行进程映射的文件对象相同 |
| ASLR/映像布局变化 | 新链接结果改变 RVA；映射基址、相邻模块、堆地址变化可显露已有 UB；同一映像的基址也可能在多进程间被复用 | 不要假定每次启动独立随机重基，更不能说“ASLR 本身造成 Rust 崩溃”；应比较模块哈希与 RVA |
| 文件对象/缓存/冷热启动 | 同字节重拷贝可能改变文件身份、页缓存和读取时序；不同时间启动也会改变线程竞争 | Windows 没有已由本案证明的“DLL 越加载越坏”的缓存；每次新进程不会天然继承上次 Dart 函数指针 |
| 已运行进程仍映射旧文件 | 替换可能失败，或路径与仍被映射的旧文件对象不同；原有模块不会因文件被复制而自动更新 | 不应只看路径、大小或修改时间；替换失败要如实记录，不绕过权限 |
| SmartScreen/MOTW | 下载来源标记、信誉策略可能影响启动流程和延迟 | Defender 缺席不等于 SmartScreen 必定缺席，但也没有理由把 SmartScreen 放到堆破坏前面 |
| 符号与 CFG | 旧 PDB/新 DLL 混用会让栈解释错误；真正 CFG fast-fail 要查子码、间接调用目标和 load config | **PDB 不参与普通运行时执行，符号不匹配本身不会制造此崩溃**；没有 PDB 也不意味着 x64 unwind 元数据一定缺失 |

本案用户已排除 Defender，不再把“关闭 Defender/杀软重试”列为方向。也没有证据需要清空系统缓存、关闭 ASLR/CFG 或改系统安全策略作为修复。

**验证实验建议（本次均未执行）。** 最有价值的第一步是分析已有 dump，不必再编译。只有现有证据不足时再增加新的运行实验。实验中的临时身份和日志放仓库外；不修改/删除真实身份，不记录配对码、私钥、spec 全文、真实文件名或完整网络诊断。

**E0：把 6 份 dump 分成可解释的异常族，成本低，信息增益最高。**

对每份建立本地表：对应 PID/启动时刻、退出码、dump 的异常码/参数、线程 ID、故障模块与 RVA、模块版本/映像标识、实际 DLL 路径和首个可靠调用栈。先找共同指令/函数，避免用绝对虚拟地址比较。已有 dump 若不能与某轮 DLL 精确对应，就保留“二进制来源未知”，不要拿当前 DLL 强行符号化。

可在 WinDbg 中执行以下分析命令；这是供操作者执行的示例，不是本次已获得的输出：

```text
.symfix C:\akdiag\symcache
.sympath+ C:\akdiag\symbols\frozen
.reload
!analyze -v
.exr -1
.ecxr
r
kv
~* k
lm
lm a @rip
ln @rip
u @rip L16
!address @rip
```

用 `.exr -1` 的目标地址再做 `!address <目标地址>`；怀疑堆时再查 `!heap -p -a <目标地址>`。mini dump 可能缺页或缺堆元数据，命令无法解析不等于没有堆问题。需要查看原始栈槽时可用 `dps @rsp L40`，但裸槽只作辅助，不能代替 `kv` 和异常上下文。完整输出留在本机私有诊断目录；报告只汇总去敏后的模块、RVA、异常种类和结论。

用匹配的 PDB GUID/age、映像时间戳/大小及归档工件核对符号；Flutter engine 也要匹配实际打包版本。若 release 原来没有 Rust PDB，先用模块+RVA/导出名分析，后续再构建带符号的 release 诊断副本。新增符号构建可能改变布局，不能假称它与原崩溃产物完全等价。

若需要重抓，在调试器中启用首次异常中断，避免只抓到最后 fail-fast：

```text
sxe av
sxe 0xc0000409
sxe ld:ak_ffi.dll
bu ak_ffi!ak_engine_free
g
```

启动调试会改变时序和堆配置；只在调试器下不崩是线索，不是通过。异常到来时先保存现场，不要忽略异常后继续运行并把后续故障当成最初根因。

预期分流：heap free/链表/写越界 → H1/E5；FFI trampoline/错误参数 → H1/H3；renderer/ANGLE/系统字体路径 → H2/E7；Tokio/MIO/CRT/TLS → H4；loader/执行地址/CFG → H6；显式 free 后访问 → H5。系统 DLL 内失败仍可能由调用者传入坏指针引起。

**E1：冻结整套工件并确认“真实引擎成功”样本，成本低。**

1. 归档一套可复现 bundle，记录 EXE、`ak_ffi.dll`、`flutter_windows.dll`、`data/app.so`、`data/icudtl.dat`、其他 native DLL 和关键资产的 SHA-256。记录 app/native 源码版本、编译设置和 Flutter engine revision；当前 `flutter --version` 不能证明历史 bundle 使用相同 engine。
2. 每轮使用固定绝对 `AK_FFI_DLL`；记录子进程实际收到的覆盖值是否存在、选择层级与 loaded module 身份。终端环境与 Explorer/快捷方式环境可能不同；设置 shell 环境不自动更新已运行 Explorer 的环境。环境值使用运行时 `Platform.environment`；`--dart-define` 不是自动等价替代。
3. 只启动一个被观察实例，按 PID 跟踪；上一轮退出后再启动。固定观察时长，例如每轮 60 秒。把“加载成功并到达 task_ready/poll”“错误卡存活”“尚未进入目标阶段”“崩溃”分开统计。
4. 一个实验块内禁止重编译、复制或替换 DLL。固定各阶段条件，用预先确定、交错的顺序执行；再单独研究“相同字节重拷贝”与“不拷贝”。对不一致哈希、拷贝失败、未知映射身份的轮次单列，不偷偷当作成功或失败。

4/4、20/20 存活都不足以定因。即使假设样本独立同分布，零崩溃时 95% 单侧概率上界在 n=4 约为 53%，n=20 约为 14%，n=100 约为 3%；本案明显有批次相关，独立假设还不一定成立。可以先每条件 30–50 次筛选，再对有意义的变化做独立批次复核；这些数字是实验建议，不是“达到次数就宣布修复”的门槛。

**E2：验证 allocator 来源，成本低，可直接缩小 H1。**

先在**实际 Flutter release 诊断宿主中保留原有惰性解析顺序**，捕获原 `_malloc`、`_free` 实际解析所得地址、解析时刻，以及最终供应模块的版本和 RVA，不仅记录字符串符号名。可通过 WinDbg `ln <地址>`、`lm a <地址>` 或 Win32 模块查询完成。只有实际使用的这组地址指向正确配套实现时，才降低原调用路径“跨 CRT 释放”这个具体子假设，但不能排除其他堆破坏。不要先提前成对解析来证明原路径配套：那会消除两次首次解析之间模块集合变化的机会；换成另一种宿主也会改变全局符号供应集合。

随后单变量替换 Dart allocator：使用已核对版本 Windows 实现的 `package:ffi` allocator，或从**同一个明确 CRT 模块实例**解析一对 malloc/free；不要再分别全局查找。提前成对解析也可作为单独实验条件，但不能与原路径的观察混为一谈。保持 Rust DLL 哈希不变，将新旧 allocator 作为同一诊断宿主内可选条件以减小额外布局差异。若只有这一改动稳定降低复现率，继续用 E0/E5 获取堆证据，不能只凭一次存活宣布跨堆释放已确诊。Rust 的 `Box/Vec` 不能接管 Dart CRT 指针。

**E3：证明或排除提前 free，成本低。**

对 `ak_engine_free` 下断点，分别标注显式 dispose 和 Dart Finalizer 的调用路径。阶段日志只记录不可逆的 handle 代号/generation、创建/释放计数及时间，不输出业务内容。崩前从未执行 free，则当前 engine-UAF 子假设明显下降；若先 free 再 poll，则追谁释放、哪一个借用者仍在调用。必要时增加一个诊断性的强 owner 保活根进行 A/B。保活不能修复显式误 dispose，也不应以永久泄漏所有 engine 作为产品修复。

**E4：保持同一个真实 DLL，用宿主开关分段进入，成本中低。**

先做一个有运行时诊断模式的宿主，只编一次。每个条件都保留相同 Flutter UI；跳过尚未启用的步骤时显示明确诊断状态。条件不是“每种改一版 Rust DLL”：

| 条件 | 允许走到的阶段 | 若从这一阶段起出现故障，优先调查 |
|---|---|---|
| A | UI，完全不加载 DLL | Flutter/runner 基线、窗口消息与 renderer |
| B | `DynamicLibrary.open` 后空闲，完全不调用导出 | loader、CRT/TLS、依赖/映像、加载改变宿主布局；此条件须确保没有隐式 engineNew |
| C | lookup-only，再独立允许 `ak_version` | 查符号/调用入口；version 有缓冲写入，要区分 lookup 与首次 allocate/copy/free |
| D | `ak_engine_new` 后空闲，不启动 task | Tokio worker、IOCP、线程/TLS、engine 分配；保留 engine 到观察结束，避免把析构混入 |
| E | `ak_task_start` 后空闲，不 poll | 身份文件/目录、指纹/RNG、本地 runtime 表；无 registry 网络 |
| F | 单次 `poll_event`，再手工允许 `task_list` | 首个事件输出、解析、复制和释放；分别记录返回点 |
| G | 周期 poll，先事件再进度，开启/关闭列表读取 | 稳态分配压力、出队路径、JSON 大小及线程时序 |

“崩溃更晚”不必然意味着最后开启的函数直接损坏内存，仍需第一异常现场。每段都记录 enter/return、返回码、容量、是否有事件、PID/TID 和单调时间；不记录 spec、任务码、身份路径或完整 JSON。特别记录 `open_before/open_after` 和日志写入成功状态，不要让“导出没有日志”继续承担加载证明。

对启动输出大小做一个小对照：固定 `cap=256` 与较大初始 cap、以及任务列表包含不同长度的**合成路径**。若只有触发 `-3` 时出错，重点检查现场 DLL 版本及重试协议。`runtime_ready` 结构很小，不能未经测量就认为它一定走了扩容。

**E5：在异常已指向堆后使用 PageHeap/Application Verifier，成本中等。**

只针对应用映像名启用，退出实验后撤销；不要对整个系统统一启用。Windows Debugging Tools 的示例命令：

```text
gflags /p /enable ak_sending_app.exe /full
gflags /p /disable ak_sending_app.exe
```

启用后在调试器中重现，结合堆验证器查看第一次非法写或错误 free。它会大幅改变内存布局/性能；不再复现不能作为无堆缺陷的证明。PageHeap 主要覆盖经过 Windows heap 的分配，不等于覆盖所有 Dart VM 自定义内存区域。后续若需 redzone/guard page 检查，应只在独立诊断 harness 里给 caller buffer 加边界，避免把检查逻辑误做成产品容错。

**E6：debug/profile/release 与三种宿主对照，成本中等。**

现有 `integration_test/app_e2e_test.dart:20`–30 明确把“缺配置、错误卡可导航”作为 PASS；`:379`–382 只要求 bootstrap 得到 service 或 error。它不否定 debug 4/4 没崩的实测，却说明“4/4”本身不证明执行了与 release 相同的 task_start/poll。先核对既有测试记录的配置分支和 native 阶段，不需要重跑测试就能发现证据差距。

后续比较时要求三个模式使用相同 DLL 绝对路径/哈希、相同有效 registry 配置、合成身份数据和相同操作。Profile 在当前 CMake 使用 release 链接设置且安装 AOT library，是补充 JIT/AOT 判断的重要对照。再做：普通 x64 原生小宿主 `LoadLibrary + 同样 C ABI 顺序`、standalone Dart AOT 小宿主、Flutter AOT。CLI 稳定不能替代前两种。

如果原生小宿主也崩，降低 Flutter 专属解释；若原生稳定而 Dart AOT 崩，提升 Dart FFI/allocator/生命周期解释；若只有 Flutter 崩，提升 engine/renderer/线程策略和模块供应集合解释。各宿主均须覆盖同一阶段及持续时间，否则对照无效。

**E7：真正切换 renderer、UI 线程策略和 RDP 条件，成本中等。**

本地已有 wrapper 明确提供：

```cpp
// 诊断方案，写在 runner main.cpp 构造 DartProject 后、创建窗口之前。
project.set_impeller_switch(flutter::ImpellerSwitch::Disabled);
// 线程策略作为另一个独立实验条件，不与 renderer 同时更改：
project.set_ui_thread_policy(flutter::UIThreadPolicy::RunOnSeparateThread);
```

对应证据是本地 `ephemeral/cpp_client_wrapper/include/flutter/dart_project.h:129`、`:147`，以及 `cpp_client_wrapper/flutter_engine.cc:23`、`:28` 将这些属性传给 C API。实际工程使用前核对 wrapper/header 与 engine DLL 是同一版本；不要只搬新 header 搭旧 engine。线程策略还可与 `RunOnPlatformThread` 作独立对照，先记录默认模式实际在哪条线程。

`FLTEnableImpeller` 在本仓库源码没有读取点，不能据“设了仍崩”断言已排除 Impeller；给 EXE 加任意命令行开关也不等价，当前 `main.cpp:25` 把参数传给 **Dart entrypoint**。必须确认启动后后端真正改变。用户的 `OpenGLESSDF` 日志说明所报告的后端路径，但不能仅凭名称就证明实际 GPU/软件驱动实现；这需要模块/渲染信息。软件回退同样可能含驱动或图形运行时代码。

在用户有本地 console 条件时再与 RDP 比较，保持 bundle 和其他设置不变。renderer 或线程条件一变就不崩，只能说明存在关联；结合故障线程栈才决定是引擎缺陷还是地址/时序把其他 UB 隐藏。针对 runner 空指针问题，可在独立调试窗口中观察 `WM_FONTCHANGE` 到达时 controller 状态，不应向其他应用广播测试消息。

**E8：把 runtime 初始化与 task_start 的本地工作分开，成本中等。**

如果 E4 在 D 就开始失败，观察 Tokio worker 入口、MIO/IOCP/CRT/TLS 栈，给 worker 命名并记录启动/退出边界；不要先把 runtime 改成“poll 才建”。没有项目自定义 DllMain/构造器，不支持直接断言 loader-lock 死锁。纯死锁一般表现为挂起，也与当前 AV 不同。

如果仅 E 开始失败，用仓库外可丢弃目录区分“首次身份创建”与“已有合成身份读取”，继续细分目录创建、指纹和 RNG。不要删除真实 device key 来试验。只在这些证据指向具体底层依赖时，才固定一个依赖/工具链版本对照；批量升级 Rust/Tokio/Flutter 无法定位原因，而且依赖升级必须按仓库要求重新验证 relay 默认值、端点路由、权限和协议兼容。

如试验单线程 runtime，必须有专用线程持续驱动它；仅把 `new_multi_thread` 换为 `new_current_thread` 然后仍立即返回，会让后台任务不再推进，不是可接受修复。

**E9：路径与 ICU 的受控对照，成本低，优先级较后。**

当前 `dll_path.dart:105`–128 是 env 原样优先、exe 旁其次、cwd 开发回退最后。env 为绝对路径且 exe/依赖完整时，单改 cwd 不应改变主 DLL 选择；仍需核对依赖 DLL 的解析路径。建议分别对照合成的 ASCII/非 ASCII、短/长路径与新/已有身份，不能一次改变 cwd、DLL、身份和配置四项。

`runner/main.cpp:20` 的 `DartProject(L"data")` 不是充分的 cwd 缺陷证据。本地 `ephemeral/.../dart_project.h:84`–89 及 `ephemeral/flutter_windows.h:86`–100 明确约定资源路径相对**可执行文件目录**；缺 DLL 时同 bundle 能渲染也反对 ICU 普遍缺失。仍应确认 `icudtl.dat` 与 engine/app.so 同包、文件未混版；不要把这项改为盲目复制另一 SDK 的 ICU 数据。普通 DLL 加载或 Rust 文件 I/O 失败通常返回可报告错误；Flutter ICU/AOT 初始化失败还可能在 Dart 启动前退出或触发 native fatal，需核对 engine 日志和异常栈，不能一概由 Dart catch 捕获，也不能用路径名代替原因。

**具体代码审查发现及边界。**

| 编号 | 源码位置 | 判定、影响与建议 |
|---|---|---|
| C01 | `flutter_app/windows/runner/flutter_window.cpp:65`–66；`:42`–44；`win32_window.cpp:137`–149 | **确定的条件性空指针解引用缺陷**。`WM_FONTCHANGE` 分支直接使用 `flutter_controller_->engine()`，前面的 if 只保护消息转发。controller 尚未赋值或已清空时若收到该消息，就可能 AV。补 controller/engine guard；目前没有此消息出现在用户现场的证据，故不认定根因 |
| C02 | `crates/ak-ffi/src/shim.rs:213`、`:221`、`:279`、`:288`；`task_ext.rs:180`；`crates/ak-ffi/Cargo.toml` 的 `rlib` | **确定的 Rust safe API 不健全性**。safe `pub extern "C" fn` 对调用者任意非空裸指针解引用/释放，safe Rust 可触发 UB。应把有指针有效性前提的导出声明为 `unsafe extern "C"`，提供明确 Safety 契约。C ABI 不变，Rust 调用点需显式 unsafe；这不会自动修复外部 C/Dart 错指针，也不证明当前 Dart 在误用 |
| C03 | `flutter_app/packages/ak_ffi_dart/lib/src/bindings.dart:72`–97 | **分配器选择不确定性**。两个 process-wide lookup 未建立配套证明；还忽略 alignment、未拒绝负 size。当前都是 Uint8 分配，alignment 不是此次已证实问题。固定一对 allocator 并核对供应模块 |
| C04 | `shim.rs:13`–14、`:26`–27、`:101`–114；`task_ext.rs:89`–102；`sched_ext.rs:617` | **输入边界承诺不实**。`CStr::from_ptr` 必须先有有效 NUL 终止内存，不能检测“给定分配内缺 NUL”。无终止符可越界读，但正常 Dart `toNativeUtf8` 明确写 NUL。修正文档；新 ABI 使用 ptr+len，但 ptr+len 同样不能识别伪造地址/虚报长度 |
| C05 | `pairing_ffi.dart:80`–100，尤其 `:89`；`ffi_json.dart:54`–59；`engine.dart:503`–522 | **输出读取防御不足**。last-error 成功后无界找 NUL；当前 shim 成功必写 NUL，未证实正常触发。利用 last-error 返回长度检查 `0 <= len < cap` 和 `buf[len] == 0`，或使用有 cap 的 decoder；旧/桩 DLL 违约时明确报协议错误 |
| C06 | `task_ffi.dart:666`–680；`pairing_ffi.dart:765`；`sched_ffi.dart:434` | **可加强契约，未发现当前正常越界**。成功无 NUL 时接受整段 cap；不校验 cap 与 Dart buffer 长度；复用 Dart buffer 时没有清尾/NUL。当前高级调用每轮新建零初始化列表，setAll 是拷贝且 finally 在其后 free。添加 cap/上限/NUL/返回码检查，别把 RangeError 误说成 native 越界 |
| C07 | `engine.dart:74`–75、`:102`、`:115`–123；`task_ffi.dart:435`–461；`main.dart:105`；adapter `:152` | **公共借用生命周期依赖外部约定**。binding 只保存裸 pointer，没有自己的 owner lease/存活检查。当前 app 通过方法闭包保活且先停借用者后 free，启动 UAF 未证实。改为持有可验证存活的 owner/lease，逐调用检查 generation/disposed |
| C08 | `main.dart:76`、`:85`–89；`ak_engine_facade.dart:168`–173；`task_ffi.dart:495`–506 | **加载来源和兼容性判定不充分**。两次解析，第一次 resolution 未作为实际加载输入；且先 engine_new 后探测 task 导出。应只解析一次，先 open/查 ABI/build ID/必需能力，再创建引擎。符号存在不保证签名/语义一致 |
| C09 | `flutter_app/windows/runner/CMakeLists.txt:49`–66；`scripts/package_windows.ps1:130`–144、`:237`–250 | **产物一致性缺陷**。仅 POST_BUILD 拷 DLL，未表达源 DLL 依赖/独立 install；包装脚本根目录取 Cargo DLL、flutter 子目录取现成 runner DLL，无一致性校验。为 native artifact 设置明确依赖和安装规则，并在发布 manifest 中绑定整套哈希 |
| C10 | `core.rs:110`–123；`shim.rs:185`–221；`lib.rs:29`–31 | **panic 保证范围被文档夸大**。guarded 主要包 safe body，engine_free 没有 catch，部分 TLS/分配/写回在外部。统一错误边界、显式 shutdown 状态与错误码；任何 catch 都不能恢复 AV/fail-fast/OOM abort，不能把它作为内存安全保证 |
| C11 | `shim.rs:1174`–1180；`task.rs:3379`–3381；`pairing.rs:916` | **错误回退过宽、idle poll 额外分配**。注释说仅 -4 fallback，实际任何错误都回退，task lookup 还吞掉所有错误；正常 task poll 先在旧 pairing 表失败并构造错误。按 runtime 类型直接分派，仅对 not-found fallback，保留内部错误 |
| C12 | `services/simple_transfer_service.dart:353`、`:673`–689、`:945`–965 | **泵没有单轮预算，错误处理不完整**。持续事件可占用 UI；持久 native 错误每 50ms 重试，解析错误/进度错误不统一呈现。加条数/耗时预算、错误计数和可见失败状态；不是本次 AV 的已确认原因 |
| C13 | `core.rs:919`–923 | **初始化错误细节丢失**。runtime build 的 `.ok()?` 让真实 OS/资源错误变成通用信息。保留类型化、去敏错误码，帮助区分线程创建失败和崩溃；普通 build Err 应继续进入错误卡 |
| C14 | `task_ffi.dart:511`–513、`:535`–538 等多分配入口 | **异常分配路径可能泄漏**。后一次分配在 try 之前失败时，前一次内存没 finally。用 arena 或嵌套 try/finally；通常涉及 OOM/异常输入，非普通首启间歇崩溃的主要解释 |

C01 可直接采用的局部修补形状如下，**本次未应用**：

```cpp
case WM_FONTCHANGE:
  if (flutter_controller_ && flutter_controller_->engine()) {
    flutter_controller_->engine()->ReloadSystemFonts();
  }
  break;
```

以下是已经核对、应避免重复误报的部分：

| 检查点 | 当前代码结论 |
|---|---|
| Rust 输出边界 | `shim.rs:71`–89、`task_ext.rs:59`–77、`sched_ext.rs:578`–596 均先检查 `text.len()+1 <= cap`，再复制和写 NUL；前提是调用者给出真实有效分配 |
| ID 长度 | `read_job`/`write_job` 是 16 字节；Dart `_idIn` 在 `task_ffi.dart:651`–660 校验长度并另分配；未发现 16/32 字节混淆 |
| 输入与后台任务 | 所阅异步任务保存 owned String/PathBuf/数组/Arc；未见保存 Dart 临时 C 字符串或输出 buffer 的裸指针 |
| `_bufCall` 生命周期 | native 调用同步返回后，bounded scan + setAll 复制到 Dart 内存，再 free；临时 asTypedList view 未作为结果逃逸 |
| `-3` | Dart 逐轮新 buffer、倍增至 16 MiB；Rust poll 短 buffer 回推事件，当前契约下没有“先越界再返回 -3” |
| ABI | 当前 task/shared 签名的参数数、顺序、i32 返回、指针一致；IntPtr 对 usize 应语义上改 UintPtr/Size，但 Windows x64 当前小正数容量同宽，不能据此声称调用栈错位 |
| 不透明类型 | AkEngine 仅通过 pointer 传递，不需要因缺少 repr(C) 就判 ABI 错误 |
| 分配/释放所有权 | engine Box 由同 DLL 创建/释放；caller buffer 由 Dart allocator 管理；问题在 allocator 供应实现是否配套，非“Rust 释放了 Dart buffer”的已发现代码 |
| 历史缺陷 | 旧 scheduler snapshot 参数错配、接收等待 dispose 后 UAF 在当前源码有修复；前者也不在此次无操作首启 task 链中。仅在证明现场实际跑旧产物时重新提升，不能把旧报告直接套到当前版本 |

**可以立即安排的修复和功能影响。** 以下是具体实施建议，尚未改码或验证；按目标分别看“健壮性提升”和“进程隔离”，避免把概率变化当已修根因。

| 方案 | 可落地内容 | 功能影响与能力边界 |
|---|---|---|
| 第一批：确定性加载与产物核验 | 单次绝对路径解析；显式记录来源层级/build ID；创建 engine 前检查 ABI/capabilities；完善 CMake 依赖/install 与整包 manifest | 正常功能保持；可能更早拒绝旧/桩/混版 DLL。主 DLL 绝对路径不自动保证依赖解析，仍须核对依赖；hash manifest 需要可信来源，不能把任意随包文本当安全证明 |
| 第一批：固定 allocator、收紧 buffer | 配套 allocator；成功 NUL/长度/容量验证；输入新 ABI 明确长度；分配失败完整释放 | 不改变协议/文件内容；ABI 演进需双方同步，旧签名不能原地悄改。防御检查不能阻止 native 已发生的非法写 |
| 第一批：runner guard、错误可见 | C01 空检查；阶段日志；保留 runtime build 错误；poll 内部错误不吞；文档去掉“任何 native 故障都可捕获”的承诺 | 正常功能影响小；改善定位与失败呈现，不等于用户崩溃已被修好 |
| 首帧后/用户动作延迟加载 | UI 先可用；在启用传输或首次生成/输入任务码时 single-flight 初始化，明确 initializing/ready/failed 状态 | 减少首帧竞争、首屏不依赖 DLL；native 崩溃仍可带走 GUI。初始化前不监听/接收；对 v1 按需任务码主要增加第一次操作等待。必须防并发点击重复创建、处理设置目录在初始化期间变更 |
| 有界加载重试 | 仅对可捕获且可判断为暂时的加载/文件占用失败，使用少量异步退避；固定所选路径 | 缺架构/缺导出/坏镜像不应盲重试；不在 AV/fail-fast 后恢复同进程；不能无限改找其他 DLL。加载已成功但引擎状态坏时按失败状态处理 |
| 绑定层 owner/lease | library、engine、buffers、runtime、dispose 归同一 owner；借用者先失效/停泵，再 shutdown/free；所有 waiters 跨 await 检查 generation | 改善生命周期安全，保留既有业务；不要让 finalizer 成为唯一关停机制，也不要通过永久禁用 free 掩盖问题 |
| 单独 Dart isolate | 一个 isolate 独占所有 FFI 与析构，UI 只发类型化消息；不传裸 engine 指针给旧 pump | 可改善 UI 响应和串行化，**不能隔离 native 进程崩溃**；增加 IPC/事件路由工作。不是“UI isolate 不崩就整个 GUI安全” |
| **本机独立 worker 进程** | GUI 从始至终不加载 ak_ffi；worker 提供 task API、文件读写和最终设备直连，GUI 用本地 IPC 控制 | **可隔离 worker 的 AV/fail-fast**；工程量较大。重启后需恢复状态并重新授权，不能伪造成功；若 GUI 自身 engine 有独立缺陷，仍需修复 |
| renderer/线程策略规避 | 只在 E7 明确显示某配置有稳定差异后，固定经过验证的 engine 设置或版本 | 可能改变性能/驱动兼容；先确认后端确实切换。不可把未证实有效的 env 开关写成永久修复，也不建议把 debug 作为产品发布替代品 |
| polling 优化 | 按平面直接路由、单轮条数/时间预算、空闲退避；后续由单 owner 复用有界 native buffer | 降低空轮询分配和 UI 卡顿；空闲退避会增加事件响应延迟，应在活跃传输/待同意时维持及时处理。优化减少复现不构成内存问题消失的证据 |

独立 worker 的实施应先冻结本地 IPC 契约，再按模块分工。当前 CLI 没有 v1 task API，不能直接把已有 `ak-cli` 命令当成无修改替代品；可增加专门 worker，内部直接使用 Rust crate，或在 worker 中加载 DLL。建议固定版本/消息长度上限、request ID、响应和事件序号、进程 generation、操作幂等语义与显式 ready 握手；Windows named pipe 限当前用户并核对对端进程，避免其他本机进程伪造“同意接收”。不要把命令字符串拼接交给 shell。

worker 崩溃后 GUI 显示“传输引擎中断”，有限重启并进入待恢复状态；不能自动重放生成码、发送、接受请求等有副作用操作。已有文件按当前检查点/校验/安全提交规则恢复，不从“最后有进度”推断完成。重新建立的连接仍要验证真实对端、真实路径和禁用 relay/proxy 配置；路径变化重新授权。接收路径越界和符号链接竞态防护继续保留。

该 worker 是**同一台终端设备的应用组件**，文件由本机 worker 与已授权最终对端直接传输。GUI IPC 传本地控制信息，协调服务器仍只处理发现、状态、必要信令与 NAT 协调；不引入 TURN、relay、HTTP 上传、云暂存、第三设备代转或隐藏代理。未知/不可信路径不进入文件数据面；无直连继续暂停/等待/重试。原始字节不压缩不转码的承诺不变。

**建议执行次序和验收证据。**

先完成 E0/E1，得到至少一个与工件一一对应的故障线程、模块+RVA 和异常参数。然后执行 E2/E3 与 E4，选择被证据指向的少数条件做进一步对照；不要同时更换 allocator、Flutter、Tokio、渲染后端和 DLL。C01、产物一致性、边界检查、错误报告可独立小改提交，但每个变更须有自己的效果和回归记录。

根因关闭应有“首次非法操作或明确初始化失败机制”的证据、一个可解释的修复，以及跨独立批次的同条件回归。只有崩溃率下降但缺乏机制证据时，应标为规避；worker 隔离则可单独验收“worker 故障不会结束 GUI”，不必谎称 native 根因已修复。

后续实施的验证门槛仍需遵守仓库规定：Python 仓库检查、Rust 测试、静态检查、Dart/Flutter 对应检查和涉及平台的实际构建；另加**真实 Flutter Windows release AOT 且真实 DLL/task 路径**的启动样本，以及配对、双向文件、逐次同意、取消/退出/重启恢复等回归。debug/Fake 测试不能替代 release 首启证据；设计符合 ADR-001 也不能替代真实身份/路径验证和抓包证据。**本次这些门槛全部未执行，原因是用户明确要求只读分析、不得构建/测试。**
