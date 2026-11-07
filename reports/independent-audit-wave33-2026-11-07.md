# wave-33（ebf402b）独立审核报告 · 2026-11-07

审核人：独立审核员（与修复执行者不同的审核实例，用户指定）。
审核对象：分支 `dev/local-parallel` 提交 `ebf402b67f0c693c6c4e07b58c34e3612f0d37e9`（wave-33：按 Codex 崩溃分析修复 C01-C14 + E4 诊断宿主）。
对照基准：`reports/codex-crash-analysis-2026-11-07.md`（Codex gpt-6-astra/max 产出）"具体代码审查发现"表（C01-C14）与 E4 实验设计（A-G 截断表）。
方法：全量 diff 浏览 + 关键修复点逐项源码核对 + 报告指定抽查（分配器残留、unsafe 导出全集、Safety 文档、CMake 生成树实证、错误文案对照）+ 实际运行 cargo/dart/flutter 测试与契约检查。未修改任何源代码，未 git commit。构建/双击稳定性未执行（按审核任务约定不必跑）。

---

## 一、逐项判定表

| 编号 | 报告要求（摘要） | 判定 | 证据与核对细节 |
|---|---|---|---|
| C01 | `flutter_window.cpp` WM_FONTCHANGE 分支补 controller/engine guard | **已正确修复** | `flutter_app/windows/runner/flutter_window.cpp:66-77`：`if (flutter_controller_ && flutter_controller_->engine()) { ReloadSystemFonts(); }`，与报告给出的修补形状逐字一致；注释如实说明消息可在 OnCreate 前/OnDestroy 后到达。 |
| C02 | 40 个指针导出改 `pub unsafe extern "C" fn` + Safety 契约；rlib safe API 不健全性消除 | **已正确修复** | 全 crate `#[no_mangle]` 导出共 41 个：shim.rs 27 unsafe + 1 safe、task_ext.rs 8 unsafe、sched_ext.rs 5 unsafe = 40 unsafe；唯一保留 safe 的 `ak_engine_new`（shim.rs:213）不接收指针参数，返回 owned handle，文档说明其安全性论证（等价 `Box::into_raw`），合理。抽查 5 处 Safety 契约（`ak_engine_free`、`ak_task_start`、`ak_task_send`、shim.rs `read_cstr`、sched_ext.rs `read_cstr`）均写明指针有效性/NUL/容量前置条件。`task.rs` 保持 `#![forbid(unsafe_code)]`（task.rs:179），测试经 `task_ext.rs:577` `#[cfg(test)] mod testabi` 的单行 SAFETY 注释适配器调用真实导出——形状干净，不泄漏到生产代码。`tests/ffi.rs`、`ffi_pairing.rs` 改动均为调用点补 `unsafe {}` 块，无逻辑变化。 |
| C03 | 分配器固定为单一配套实现 | **已正确修复** | `bindings.dart:88`：`final Allocator processAllocator = malloc;`（`package:ffi`，Windows 即 ole32 的 CoTaskMemAlloc/CoTaskMemFree 配对）。全包 grep 无任何 `DynamicLibrary.process()` 分配器查找残留（仅 `diag_stage_test.dart` 测试 fake 用 process() 充当 library，与分配无关；`ffi_safety_test.dart` 是断言文本）。`pubspec.yaml` 新增 `ffi: ^2.0.0` direct main 依赖，`pubspec.lock` 同步 `dependency: "direct main"`。新测试 `expect(identical(processAllocator, malloc), isTrue)` 直接锁定单一实现对象。 |
| C04 | NUL 终止是前置条件、不可校验——文档诚实化 | **已正确修复** | shim.rs 模块文档与 `read_cstr` Safety（shim.rs:112-127 附近）、task_ext.rs `read_cstr`、sched_ext.rs:610-617 三处一致改为："`CStr::from_ptr` 向前扫描首个 NUL，无长度可查，缺失终止符是越界读 UB 而非可返回的错误；NUL 是严格调用方前置条件"。不再声称"校验 NUL 终止"。 |
| C05 | last-error / version 输出读取有界 | **已正确修复** | `ffi_json.dart` `fromNativeUtf8(pointer, capacity, {length})` 单一实现双形式：长度形式校验 `0 <= length < capacity` 且 `pointer[length] == 0`；扫描形式 `_scanNul` 最多扫到 capacity-1，无 NUL 抛 `AkFfiProtocolException`。`pairing_ffi.dart` `readEngineLastError` 与 `engine.dart` `_charOutLoop`（ak_version/ak_engine_last_error）均传 `length: code`。**无 off-by-one**：limit==capacity-1（cap-1 字节载荷+NUL）合法；`utf8.decode(asTypedList(limit))` 只读 [0, limit)。 |
| C06 | `_bufCall` 契约（cap==buffer.length、成功 NUL、-3 清尾） | **已正确修复** | `pairing_ffi.dart` 新增共享 `akBufferCall`（pairing/task/sched 三层共用）：cap 与 Dart buffer 长度不符立即 `ArgumentError`；正码要求 cap 内存在 NUL，否则协议异常（不再吞整段 cap）；成功把载荷+显式 NUL 写回 Dart buffer（复用缓冲无残留）；-3 将 buffer 清零；其他负码不动缓冲。cap==0 安全（分配 1 字节）。 |
| C07 | 借用生命周期改为可验证存活的 owner/lease，逐调用 generation/disposed 检查 | **未修** | `AkTaskFfiBindings`（task_ffi.dart:437-464）仍只保存裸 `Pointer<AkEngine> _engine`，无 lease/generation/逐调用存活检查；engine.dart 的 `_disposed`/`isDisposed` 是修复前既有代码；全库 grep 无任何 C07 引用或新增 lease 机制。报告自己也说明"当前 app 通过方法闭包保活且先停借用者后 free，启动 UAF 未证实"——未修不构成已知崩溃风险，但属于报告发现中未关闭的一项。 |
| C08 | 单次解析→绝对路径→open→探测导出→才 engine_new | **已正确修复** | `main.dart`：resolve 一次 → `absoluteAkDllLoadPath()`（非 bundled 层 `File(path).absolute` 冻结绝对路径；Android bundled SONAME 保留原样，理由正确）→ `AkEngineDartFacade(dllPath:)` → `openLibrary()`（内部 `DynamicLibrary.open` 一次并复用）→ `probeTaskExports()`（缺导出则 dispose 并报错，**不创建引擎**）→ `engineNew()`（`AkEngineDart(bindings: AkFfiBindings(library: …))` 复用已打开句柄，无第二次 open、无二次解析）。旧的双解析（facade 内再次三级探测）已消除——`_dllPathForOpen` 注入路径优先。real-DLL 条件测试断言 `identical(engine.bindings.library, facade.library)` 与日志相位顺序（open 在 probe 内部）。 |
| C09 | CMake 显式依赖+每次构建校验；打包脚本单一权威源+哈希 | **已正确修复（实证）** | `windows/runner/CMakeLists.txt`：`add_custom_target(ak_ffi_bundle ALL)`（无输出文件→每次构建都跑）`copy` + `compare_files` 逐字节校验 + `DEPENDS 源DLL`（源消失=构建失败）；命令引用 `$<TARGET_FILE_DIR:ak_sending_app>` 使 CMake 自动加 ak_ffi_bundle→runner 顺序依赖（复制在链接后，等价旧 POST_BUILD 时机；runner 不反向依赖，无环）。另加同源 `install(FILES)`。**生成树实证**：`build/windows/x64/runner/cmake_install.cmake` 中 genex 已正确展开为 Debug/Profile/Release 三配置绝对路径，`install_manifest.txt` 含 Release DLL——本机 VS 生成器下真实生效。`scripts/package_windows.ps1`：cargo 产物为权威源覆盖 flutter 子目录副本 + 对两份打包副本做 SHA256 比对，不符即 Fail。仓库内两份（`target/release/ak_ffi.dll` 与 `build/.../Release/ak_ffi.dll`）实测哈希一致（d62904a3…，时间戳 21:07 晚于提交正文所述 19:19 旧件，"stale artifact"发现可信）。 |
| C10 | panic 包含范围文档去夸大 | **已正确修复** | core.rs 模块文档与 `guarded` 文档、shim.rs 模块文档、lib.rs 文档：如实声明 catch 只覆盖 safe body；指针适配器、last-error TLS 记账、`ak_engine_free` 析构路径在 catch 外；fail-fast（AV/abort/OOM abort）不可捕获；是错误报告而非内存安全承诺。与代码实际结构一致。 |
| C11 | 共享 poll/op/cancel 按 runtime plane 直派；仅 not-found 回退；消除 idle poll 无谓分配 | **已正确修复** | `pairing.rs:375` 新增 `runtime_plane`（单次 v2 锁内 pairing 表→task 表判定）；`ak_pairing_op_result`/`ak_pairing_poll_event`/`push_pairing_or_task_event_front`/`ak_pairing_cancel_op` 四处全部改为单次判定直派，平面自身错误（含锁中毒 -5）原样传播，仅双表 miss 才 -4。**错误码/文案兼容核对**：旧路径双表 miss 最终返回的即 `runtime_of` 的 `not_found("unknown runtime {hex}")`——新路径生成同款文案；已知 runtime 的 "unknown op {} on runtime {}"（-4）由 owning plane 原样给出，与旧行为一致；task.rs 新增 `task_op_result`/`task_poll_event`/`task_push_event_front`/`task_cancel_op`（内部锁中毒 `-5 internal "…lock poisoned"` 传播，不再被 pairing 侧查找掩盖）。idle task poll 不再先构造并丢弃 pairing not-found（每次空轮询省一次 format 分配）。`*_maybe` 投机函数已全部删除。 |
| C12 | `_pumpOnce` 加单轮条数/耗时预算、错误计数与可见失败状态 | **未修** | `flutter_app/lib/services/simple_transfer_service.dart` 在 ebf402b 中**零改动**（最后修改为 wave-31 df7d4ee）。`_pumpOnce`（:945-965 区域）仍是 `while (true)` 无预算 drain；`on AkFfiException { return; }` 仍是每 tick 静默重试。全库无 C12 引用、无任何预算实现。报告对该项的定性（持续事件可占 UI、持久错误每 50ms 重试）当前依然成立。 |
| C13 | runtime build 错误保留 OS 原因 | **已正确修复** | core.rs：`AkEngine::new() -> Result<AkEngine, std::io::Error>`（`.build()?` 原样携带 io::Error）；shim.rs `ak_engine_new` 的 `map_err` 把 OS reason 拼入 last-error（"cannot build the embedded tokio runtime: {error}"），不再 `.ok()?` 折叠成通用消息。 |
| C14 | 多分配入口 arena 化，中途失败不泄漏 | **已正确修复（一处备注，见新问题 P3-3）** | pairing_ffi / task_ffi / sched_ffi / engine.dart 的全部多分配入口（start/_specStart/send/opResult/pollEvent/cancelOp/peerAdd/peerRemove/…）改为 `using((arena){…}, processAllocator)`，id/spec/out 缓冲全部 arena 分配；`_idIn` 收 Arena 参数。测试 `_CountingAllocator(failOnNth: 3)` 实证第 3 次分配失败后前两次分配被释放（liveAllocations==0、frees==2）。提交声称 20 个入口，抽查覆盖 4 个文件的代表入口，未见遗漏模式。 |
| E4 | AK_DIAG_STAGE A-G 截断宿主（A 不加载 / B 仅 open / C 仅 lookup / D 建引擎后停等保活 / E task_start 后不 poll / F 单次 pump 后封停 / G 全路径） | **已落实（两处实现取舍，见下）** | `main.dart`：A 直接返回 Unusable（不 resolve 不加载）✓；B `openLibrary()` 后 `_diagHalt`（openLibrary 内部无任何 lookup/调用，仅 log）+ keepAlive 保活✓；C `probeTaskExports()`（纯 lookup）后停 ✓；D `engineNew()` 后 halt，`UnusableSimpleTransferService.keepAlive: owner` 保活到观察结束、dispose 刻意不释放 ✓；E `DiagTaskCallsGate(startOnly)`：仅 task_start 放行（其同步启动泵的 poll/list 抛 `AkFfiException(errInternal,…)`，被服务层既有 catch 吞掉，shell 存活且无 native poll）✓；F：`_engine.start()` 全同步 body（task_start + poll_event×2 + poll_progress + task_list）返回后 `gate.seal()`，时序经 `_engine.start()` 无 await 的代码路径验证，后续 50ms tick 全部拒绝 ✓；G 全路径；无效值降级 G；Android E/F 不截断并横幅注明。widget 测试对 E/F 的 native 调用序列逐项断言（E: `['task_start']`；F: `['task_start','poll_event','poll_event','poll_progress','task_list']`，seal 后 200ms 无新增）。取舍①：报告 E4 表 C 行的"再独立允许 ak_version"未实现（C 即 lookup 后停，是报告可选项的简化）；取舍②：F 的"单次同步 pump"包含 poll_progress（即真实启动序列全貌），比报告 F 行字面（poll_event+task_list）略宽——两者均为合理实现选择，横幅与文档如实描述实际深度。 |
| E2 | 阶段毫秒计时日志 | **已落实** | `AkStageLog`：`%TEMP%\ak-stage.log`（TEMP/TMP/TMPDIR 层级、分隔符处理有测试），`writeAsStringSync(mode: append, flush: true)` 同步落盘；写失败自禁用、永不抛、不扰动启动。生产路径每次运行也记录 open/probe/engine_new 相位（提交声明的 "stage timing log on every run"，有意行为，见 P3-2 备注）。 |
| 加固项 | `sched_cancel_stops_file_transfer_growth` 64→16 MiB，取消语义不变 | **语义保持** | diff 仅改 payload 大小、写入提示与注释；所有断言（cancel 后目标树停止增长等）零改动；测试仍为真实回环传输中途中断。 |

C09 部署侧实证补充：`dist`/部署目录在仓库中不存在，提交声明的"3-way hash identity"第三方（部署副本）无法在本审核中独立复核；仓库内两份哈希一致已实测（见 C09 行）。

---

## 二、发现的新问题（按严重度分级）

**P1（诚实性缺陷——提交信息失实）**
- 提交标题与正文首句声称 "close **all 14** Codex crash-analysis code findings (C01-C14)"，实际 **C07、C12 两项完全未修**：`services/simple_transfer_service.dart` 零改动（C12），borrow 层无任何 lease/generation 机制（C07），全库无这两项的引用。提交正文的具体条目列表事实上未包含 C07/C12 的任何修复描述（正文只列了 12 项），即作者大概率知道未修，但标题结论与"全部关闭"的声明不实。**要求**：更正提交信息（或在下一 wave 补齐 C07/C12 后再引用本轮）。这不影响已修 12 项本身的正确性。

**P3（低风险观察，不阻塞）**
1. E4 诊断保活：B/C/D 阶段 `keepAlive` 使已打开的库与已建引擎不随 app 关闭释放（dispose 刻意 no-op），依赖进程退出回收——诊断宿主的有意设计（观察窗口不得混入析构），但使用者应知晓诊断版关闭时无显式 engine_free。
2. `AkStageLog` 生产路径每次启动追加约 6 行到 `%TEMP%\ak-stage.log`，无轮转/上限（每次启动约 200B；E2 声明即"every run"，属有意；多实例并发追加失败会自禁用该实例日志，不崩溃）。
3. C14 泄漏测试用 `_simulateStartEntry` 复刻入口的分配形状而非直接驱动真实绑定方法——若将来某入口去掉 arena，该测试不会报警（canary 测试部分缓解；1000 轮 canary 在真实跨堆错配上是合理性探测而非确定性检测，CoTaskMem 堆损坏可能潜伏到之后才爆）。
4. C09 的 CMake 复制改为无条件 `copy`+`compare_files`（非 `copy_if_different`）：每次构建强制复制，若上一个 debug 实例仍在运行锁定 DLL，构建 loudly 失败——符合"fail loudly"意图，但会打断日常迭代（与旧行为同类失败模式，非回归）。
5. `install(FILES … DESTINATION "$<TARGET_FILE_DIR:…>")` 依赖 CMake 对 DESTINATION 中生成器表达式的支持；本项目 `cmake_minimum_required(3.14)` + 本机 VS 生成器已实证正确展开（三配置 + manifest）。若未来更换生成器/极老 CMake 版本或 policy 走向（CMP0177 一系）变化需留意；当前证据充分。

**未发现的缺陷类（明确排查过）**：bounded read 无 off-by-one（长度/扫描两形式边界均正确）；arena 用法正确（`using(…, processAllocator)` 显式单分配器，无混用）；诊断门在 `AK_DIAG_STAGE` 未设置时完全不介入正常路径（gate==null、无横幅、无截断；仅 E2 阶段日志照写，为声明行为）；C11 无错误码/文案回归；40 导出 unsafe 化对 crate 内测试/Fake 干净（tests 仅补 unsafe 块；Dart `FakeAkTaskCalls` 实现的 `AkTaskCalls` 接口未变）。

---

## 三、验证复核输出（本机实测，2026-11-07）

| 命令 | 结果 | 与提交声明比对 |
|---|---|---|
| `cargo test --workspace` | **1132 passed / 0 failed** | 声明 workspace 1132/0 ✓ 一致 |
| `cargo test -p ak-ffi`（分项） | 51+29+24+0 = 104（含集成/单元/doc） | 含于上项 ✓ |
| `cargo clippy -p ak-ffi --all-targets` | 无警告输出 | 声明 clippy clean ✓ 一致 |
| `python -I scripts/check_contracts.py` | **5/5 PASS**，导出计数 41（sched 5/shim 28/task_ext 8） | 声明 contracts 5/5 ✓ 一致；41=40 unsafe+1 safe，与 C02 改动自洽 |
| `dart test`（ak_ffi_dart 包） | **157 全部通过**（All tests passed!） | 声明 dart 157/157 ✓ 一致 |
| `flutter test`（flutter_app） | **155 全部通过**（All tests passed!） | 声明 flutter 155/155 ✓ 一致 |
| 哈希（仓库内两份 DLL） | `target/release/ak_ffi.dll` == `build/.../Release/ak_ffi.dll`（d62904a3…，均为 21:07） | 与"stale 19:19 修复 + 构建部署"叙述吻合 |
| 50/50 双击、部署副本（第三份哈希） | **未复核**（运行时验证，按任务约定构建/双击不必跑；部署目录不在仓库内） | 无法证实亦无法证伪，如实记录 |

新增测试有效性抽读（3 个，均真实有效）：
1. `diag_stage_test.dart` stage E/F widget 测试：fake owner + RecordingTaskCalls 断言精确 native 调用序列，并在 seal 后泵 200ms 验证不再触达 native——直接锁定 E4 截断语义。
2. `ffi_safety_test.dart` C14 组：`_CountingAllocator(failOnNth:3)` 实证 arena 在中途失败时释放先前全部分配（liveAllocations==0/frees==2）与正常路径进出平衡。
3. `ffi_safety_test.dart` C03 组：`identical(processAllocator, malloc)` 锁定单一实现对象 + 1000 轮变长 alloc/write/free + canary 堆健康探测。

---

## 四、总结论

**有条件通过（CONDITIONAL PASS）。**

- C01-C06、C08-C11、C13、C14 共 **12/14 项已正确修复**，E4/E2 诊断宿主已按报告落实（两处声明的实现取舍），16 MiB 加固保持取消语义；修复本身**未发现引入新缺陷**（重点排查的 arena 误用、bounded read 边界、CMake 兼容、诊断门泄漏、错误文案回归均为阴性或仅 P3 观察）。
- 全部可静态/测试复核的声明（workspace 1132/0、dart 157、flutter 155、clippy、contracts 5/5）**与实测完全一致**；50/50 双击与部署哈希属运行时验证，未复核。
- **条件**：提交信息 "close all 14 (C01-C14)" 失实——**C07（owner lease/generation）与 C12（泵预算）未修**且均无任何代码痕迹。需更正该提交的声明（或补齐两项修复）后方可按"全部关闭"对外引用；C07/C12 本身按原报告均为防御性加固（未证实与本次崩溃相关），不阻塞本轮其余修复的合入价值。
