# M0-05：Flutter 风险实验壳开发后审核

日期：2026-11-02（America/Los_Angeles；最终 CI/复审 UTC 日期为2026-11-03）。
**工程子范围 REVIEWED；模块总验收 PARTIAL_DEVICE_GATES_NOT_RUN。整个 M0 未完成。**
[先行计划](../docs/modules/M0-05_PLAN.zh-CN.md) · [机器证据](M0-05_VALIDATION.json) · [设备矩阵](../experiments/flutter_probe/DEVICE_MATRIX.zh-CN.md)。

## 1. 提交与修改范围

先行计划 `8edf293301b94684d95e81104e89a3c524ca0c43` 先于实现；首实现 `1ada78ab630a5cc608fa63a9f63056901f35f2e5`；iOS两项P2修复 `b4a3d5ed048c23d3505ca3e22b313cf50118d9f3`；macOS ABI修正 `c88cac0f52ad0c03699d5bc03db0bdf8f0ef4b37`。
最终受测且独立复审为 `66555214e40e779013c07cb0bda3d6dcf6bd5332`，树 `3bcdfa7574c5bf4a51c74c23e49d564314219319`。后续交接提交只改文档/证据/摘要，不冒充被审代码。PR #6保持草稿，不合并main或其他PR，不部署、不采购、不强推。

范围：隔离Flutter实验壳、固定SDK生成工程及真实锁文件、标量FFI/5Hz状态/取消释放、Android权限与SAF实验、iOS书签/安全作用域/协调访问实验、五端发布构建和证据脚本。生产core/transport、认证配置和依赖没有因本模块新增UI、媒体或中继能力。

## 2. 计划逐项验收

| 计划 | 结果与边界 |
|---|---|
| A1 回归 | 三桌面生产108项Rust、原生实验6项Rust均通过；Linux/macOS 45项Python通过，Windows 43项通过+2项Unix符号链接测试明确跳过；5个主机构建任务各执行7项Dart测试 |
| A2 真实FFI | 三桌面通过Dart实际加载原生库并完成3次同机UDP/TLS循环及取消/释放；fake组件测试只检查交互，不冒充网络 |
| A3 五端release | Windows x64、Linux x64、macOS arm64、Android arm64、iOS arm64构建成功；native库嵌入/摘要、Apple单一arm64和5个FFI导出检查通过；Android测试密钥签名，iOS未签名 |
| A4 手机目录与生命周期 | Kotlin/Swift平台代码已在App构建中编译；同一iOS文件操作helper的6项真实主机文件回归通过；物理设备目录/权限/后台/进程实验仍NOT_RUN，故本模块不能标总体验收完成 |
| A5 固定输入和证据 | Flutter commit/engine固定，pubspec.lock受约束，Cargo --locked预取后离线；源代码clean；生成工程来源保留；失败历史保留 |
| A6 审核 | 实施者复核和真实独立复审完成，最终回执与受测SHA相同；无已知未修复代码阻断，不等同专项安全审计或正式APPROVED |

## 3. 审核发现与修复

| 问题 | 修复和回归 |
|---|---|
| 独立P2 #4443325533：iOS部分写入失败遗漏自己创建文件的清理 | O_EXCL/O_NOFOLLOW排他创建后登记所有权；异常也清理；名称冲突和已知inode替换不删除既有内容；Swift真实文件回归 |
| 独立P2 #4443325550：进入协调块后后台/超时取消不能阻止后续读取 | 线程安全取消标记，在创建/写/读及完成处检查；取消仍清理，线程未退出不解除busy；写入中取消回归 |
| 独立P1 #4443495910：lipo日志的command元数据被当作架构输出 | 核对command JSON与实际argv，再只解析工具输出；严格单一arm64；nm要求精确T定义符号；真实run_one、LF/CRLF、空/错误/多架构及假符号回归 |
| Android固定SDK安装名错误 | 根据实际sdkmanager清单改用platforms;android-37.0；compile/target仍37，AGP仍9.1.1，不降级、不跳过平台；保存安装及source.properties |
| 实施者复核：Apple bundle ZIP可能丢失framework链接与执行权限 | 保存内部符号链接/文件模式/空目录，拒绝逃出产品根目录的链接；Unix真实归档回归；不跟随外部链接 |

最终独立执行者 `chatgpt-codex-connector[bot]`（199175422），[回执6592174236](https://github.com/wmqfl861/AK-sending/pull/6#issuecomment-6592174236)明确审查66555214e4，未发现重大问题；汇总6579099085为completed、无未解决发现。请求评论不是审核完成证据。

先前41460994948的Apple判定失败与Android安装失败保留。原SDK固定/macOS双ABI失败也保留在提交和PR记录。本地新增归档测试曾发现ZipInfo API不支持参数的错误，修正后才提交，未弱化测试。最终构建不采用android-37兼容目录链接，不更换固定SDK和工具链来隐藏错误。

## 4. 最终实际测试

- [Flutter CI 41553627431](https://github.com/wmqfl861/AK-sending/actions/runs/41553627431)：五端release全部成功；每任务7项Dart主机测试；三桌面真实Dart FFI；两个Apple主机任务各6项Swift文件回归。不能把重复执行当成不同测试。
- [Core CI 41553627658](https://github.com/wmqfl861/AK-sending/actions/runs/41553627658)：三桌面每系统108项Rust、fmt、clippy、源码clean与依赖图通过；Python计数见上表。
- [Native CI 41553627512](https://github.com/wmqfl861/AK-sending/actions/runs/41553627512)：三桌面6项native Rust及ctypes真实循环，Android ELF/16KiB，iOS原生链接通过。
- [Mobile core CI 41553627432](https://github.com/wmqfl861/AK-sending/actions/runs/41553627432)：两移动目标库编译检查成功，仍不是设备运行。

13份最终artifact已实际下载；核对ZIP SHA-256、每份results.json精确commit、所有记录命令退出码和每个日志摘要、发布产品/原生库摘要。五份source.zip逐文件按Git属性清理后的blob与受测树核对一致；Windows CRLF归档不与Unix原字节摘要混为一谈。Android的ELF对齐派生检查在本地从原始readelf日志再执行一次。

本轮续接容器实际运行45项Python、静态检查，并加载最终Linux CI原生库完成3次TLS循环及取消/释放。没有在本轮容器编译Rust或运行Flutter；这些结果来自上述真实CI，不把先前会话的本地工具链记录改写成本轮执行。

## 5. 轻量化的首批构建基线

| 平台 | 产物字节 | MiB | 口径 |
|---|---:|---:|---|
| windows | 12,001,356 | 11.45 | release 目录归档 ZIP |
| linux | 10,244,841 | 9.77 | release 目录归档 ZIP |
| macos | 9,953,906 | 9.49 | release 目录归档 ZIP |
| android | 19,188,337 | 18.30 | 测试密钥签名 APK |
| ios | 8,313,338 | 7.93 | 未签名 .app ZIP |

只是这个极小实验壳的构建产物，不是完整产品包体、不代表商店下载量/安装占用，也不能代替CPU、手机PSS/physical footprint、冷启动或活动缓冲预算。macOS仅arm64，不宣称Intel兼容；iOS未签名ZIP不是可直接安装的IPA。原始资源模板的未测值保持null。

## 6. 剩余风险与交接

当前FFI传输实验仍固定127.0.0.1：**独立两设备LAN驱动尚需按新模块先行计划实现**，不能把设备未接入当作唯一剩余事项。无物理Android/iOS接入，无真实LAN权限允许/拒绝/撤销、文件提供者/目录持久授权、后台/锁屏/杀进程、最低系统及资源预算证据。iOS文件helper的inode检查不宣称能阻止所有跨进程竞态；它不是M1生产文件接收引擎。

平台调用或文件提供者可能不立即响应取消；实验保持busy、防止叠加任务，但不承诺强制停止已进入的系统调用。自动接收只存在于合成本机fixture，不能复制为产品策略。没有用户文件收发、磁盘提交、恢复、NAT或自动LAN优先选路。

下一步在不改变M0验收门槛的前提下，先为跨设备LAN探针制定独立计划，再补实验入口与实际设备矩阵。全阶段状态见 [M0总表](M0_STATUS.zh-CN.md)。回退仅实验壳/生成器/专项CI，不改ADR-001和生产core/transport。artifact保留14天；交付包保留原始文本日志，过期后不声称远端ZIP仍可下载。
