# M0 Flutter 风险实验壳

这是隔离的工程实验，不是文件传输产品。启动时不自动联网；点击“运行合成 TLS 实验”只在本机两个 UDP 端点之间交换合成数据。它不能证明两台设备、局域网权限、NAT 或真实文件保存。

## 构建

先使用 config/toolchains.json 指定的 Flutter/Rust/SDK。执行 `python scripts/run_flutter_validation.py --platform linux --output validation/flutter-attempt`（平台可选 linux/windows/macos/android/ios）。目标 SDK、linker 和依赖需按 `.github/workflows/flutter-probe.yml` 配置。输出目录和 `target/flutter-probe` 必须为空；保留失败证据后再另建干净工作区，脚本不会自动删除已有工程。

平台工程由精确 Flutter commit 生成，再应用本目录已提交源码和 `prepare_flutter_probe.py` 的受检查补丁；不把生成工程冒充已提交源码。真实 pubspec.lock 来自只读候选 CI 41455176261，正式 pub get 强制锁文件，构建后再次校验锁文件和源文件未变化。候选不是验收。

Android 沿用模板 Gradle9.3.1，将 AGP9.1.0 最小升级到9.1.1以支持API37；JDK17、NDK29.0.14206865、build-tools36.0.0，首轮仅arm64。APK使用模板本地调试测试密钥，绝非生产签名。iOS产物无签名，不能直接安装；macOS/Windows/Linux产物也没有发布签名或公证保证。

Dart只通过固定宽度整数FFI控制一个原生任务，活动时5Hz状态检查，闲置停止轮询。cancelled不等于线程已退出；只有release成功才能开始下一任务。进入后台会取消，回前台不自动重启。热重启遗留未知句柄时不接管任务，要求完整重启进程。不得在原生任务仍存活时卸载库。

## 目录实验与限制

“选择目录”不写入；“创建、校验并删除测试文件”才在用户指定目录创建唯一命名的64字节新文件，读回自己的文件并删除。不遍历目录、不读现有文件、不覆盖已有文件。Android使用系统SAF持久URI，iOS使用系统安全作用域URL及本机bookmark；这些敏感值只存本机，不返回Dart、不写日志/证据。

文件提供者可能远程或阻塞。超时会取消操作并报告失败，工作线程未退出时保持busy，不能谎报资源释放；系统/提供者不一定即时响应取消。杀进程、断电或提供者删除失败可能留下 `ak-m0-probe-<随机ID>.tmp`，须在测试目录手动检查清理，不能声称事务回滚。“忘记”仅撤销本应用保存的使用入口，不代表iOS系统全局撤权。

Android37显式请求本地网络权限；拒绝不降级或绕过。iOS loopback-only不代表本地网络权限已批准，真实LAN提示需另一个真实LAN实验。应用代码编译通过不代表权限语义通过。

## 必须补的物理设备证据

按 `DEVICE_MATRIX.zh-CN.md` 逐项记录设备/OS/App摘要、操作和脱敏原始结果；没有实际运行的格子保持NOT_RUN/BLOCKED。不得记录真实路径、URI、bookmark、证书或密钥。包体压缩字节不是商店下载/安装大小；资源测量使用 platform_probe 的模板。组件测试fake仅检查界面逻辑；actual-dart-ffi日志才是合成网络实跑。
