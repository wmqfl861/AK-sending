# ak_sending_app — AK-sending Flutter 端（Windows 桌面 + Android）

AK-sending 的 Flutter 应用壳：Material 3 + NavigationRail（主页 / 接收 /
发送 / 历史 / 关于），传输能力经 FFI 调用仓库内 Rust 引擎（`ak-engine`），
无中继、无云端暂存。

## 架构

```text
lib/ui/** 页面与壳（并行 UI 波次）
   │  只依赖 ui/service/transfer_service.dart（UI 契约：DeviceInfo/ReceiveOptions/
   │  SendOptions/JobController 会话/status 通知）
   ▼
lib/services/ffi_transfer_service.dart   ← 契约的权威实现（会话、状态机、
   │                                       事件折叠进 JobController、终态轮询）
   ▼
lib/services/real_transfer_service.dart  ← 核心服务：job 注册表（32-hex id ↔
   │                                       16 字节 FFI id）、Timer 轮询泵把
   │                                       ak_poll_progress 队列转为广播事件流
   ▼
flutter_app/packages/ak_ffi_dart         ← 纯 Dart FFI 绑定（9 个 ak_* 导出、
   │                                       spec 镜像校验、-3 自动扩容、
   │                                       19 种进度事件模型、JobController）
   ▼
ak_ffi.dll（cargo build -p ak-ffi --release）
   ▼
crates/ak-engine（内嵌 tokio 运行时，句柄式 C ABI，契约见 docs/adr/003）
```

关键文件：

| 文件 | 职责 |
|---|---|
| `lib/main.dart` | 入口：构造 `FfiTransferService` 交给 `ui/app.dart` 的壳 |
| `lib/services/dll_path.dart` | DLL 探测（Android tier-0 内置 so + 桌面三级；纯函数，可单测） |
| `lib/services/transfer_service.dart` | 核心层抽象 + `DeviceInfo` 波次边界 + 异常 |
| `lib/services/real_transfer_service.dart` | `EngineBackend` 注入缝、轮询泵、事件路由、单例 |
| `lib/services/ffi_transfer_service.dart` | UI 契约实现（会话/准入/停止/历史 + 安卓目录适配） |
| `windows/runner/CMakeLists.txt` | 构建时把 `ak_ffi.dll` 拷到 exe 旁（见下） |
| `android/app/build.gradle.kts` | Android 平台：minSdk 24（NDK android24 对齐）、applicationId `dev.ak.ak_sending_app` |

## 构建（Windows）

前置：Flutter stable、VS BuildTools（C++ 桌面开发）、Rust 工具链。

```bash
# 1. 先产出引擎 DLL（仓库根执行）
cargo build -p ak-ffi --release
#    → <仓库根>/target/release/ak_ffi.dll

# 2. 构建 Windows 发布版（工作目录 flutter_app）
export PATH="$USERPROFILE/flutter-sdk/bin:$PATH"
flutter build windows --release
#    → build/windows/x64/runner/Release/ak_sending_app.exe + ak_ffi.dll（CMake 拷贝）
```

### DLL 三级探测（运行期，`lib/services/dll_path.dart`）

`FfiTransferService` 打开引擎时按以下顺序解析 `ak_ffi.dll`，取第一个命中：

1. **环境变量 `AK_FFI_DLL`** — 显式覆盖，原样使用（不检查存在性，写错会在
   引擎卡上以加载错误原文报出）；
2. **可执行文件同目录** `<exe 目录>/ak_ffi.dll` — 生产布局（下方 CMake 步骤
   就是往这里拷）；
3. **开发相对路径**（相对进程 CWD 的 `target/release`）：
   `../target/release/ak_ffi.dll`（CWD = flutter_app：`flutter run`/`test`/
   `build`）与 `../../../target/release/ak_ffi.dll`（CWD =
   packages/ak_ffi_dart，与该包自身默认一致）。

三级全部未命中时构造不崩溃：状态点变"服务异常"，版本卡显示探测过的全部
候选路径与修复提示；`startReceive`/`startSend` 抛 `StateError`。

## 构建（Android，wave 13）

前置：Android SDK/JDK（`JAVA_HOME` + `ANDROID_HOME`，集成者环境中
`~/android-toolchain`）。`android/` 为 `flutter create --platforms=android
--org dev.ak .` 生成的标准壳：minSdk 24（与 NDK clang 的 android24 目标三元
组对齐）、`INTERNET` 权限（LAN 直传必需，release 只从 main manifest 获得）。

```bash
# 0. 引擎 so：交叉编译后放入 jniLibs（本波由集成者侧产出）
#    android/app/src/main/jniLibs/<abi>/libak_ffi.so   （arm64-v8a / armeabi-v7a / x86_64）

# 1. 构建（工作目录 flutter_app）
flutter build apk --debug
```

### Android 平台差异（服务层吸收，UI 无感知）

- **so 加载（tier 0）**：`libak_ffi.so` 随 apk 打包，`DynamicLibrary.open
  ('libak_ffi.so')` 走 System.loadLibrary 语义（OS linker 在应用 native 库
  目录解析）；`AK_FFI_DLL`/exe 同目录/CWD 探测全部跳过。
- **身份目录**：无 `Platform.environment` 家目录概念，构造时经 path_provider
  异步解析 `<应用支持目录>/.ak-sending`（`/data/user/0/dev.ak.ak_sending_app/
  files/.ak-sending`）；`refreshIdentity`/`listJobs` 在首次引擎读取前等待解析
  完成，用户 `setIdentityDir` 显式覆盖优先于解析结果。
- **接收默认输出目录**：`getDownloadsDirectory`（Android 上是应用专属外部
  `Android/data/<pkg>/files/Download`，path_provider ≥2.1 语义，非公共
  Downloads）→ `getExternalStorageDirectory` + `Download/` → 应用文档目录 +
  `Download/`；接收页异步预填，桌面仍手输路径。安卓 10+ 分区存储下公共
  Downloads 不可任意写入——导出到公共位置（SAF/MediaStore）为后续波次。
- **依赖**：`path_provider ^2.1.0`（ADR-002 受控新增，首个第三方运行时依赖：
  安卓无可注入的家目录/存储根，只能经插件平台通道取得）。

### DLL 随构建分发（`windows/runner/CMakeLists.txt`）

runner 目标 POST_BUILD 拷贝，来源优先级（可在 configure 时覆盖）：

1. CMake 缓存变量 `-DAK_FFI_DLL=C:/path/ak_ffi.dll`；
2. 环境变量 `AK_FFI_DLL`（configure 时刻读取）；
3. 默认 `<仓库根>/target/release/ak_ffi.dll`（相对 runner CMakeLists 三级
   上跳）。

全部缺失时仅 WARNING（构建照常成功，应用启动后处于无引擎状态）。

## 测试

```bash
flutter test test/services_test.dart    # 服务层：探测顺序、事件路由（Fake 驱动）、
                                        # 适配器会话语义、真实 DLL 冒烟（缺失自动 skip）
flutter analyze                          # 静态检查
```

`packages/ak_ffi_dart` 自身还有 69 项纯 Dart 测试（在包目录 `dart test`）。

## 当前边界（wave 11）

- **单机验证**：UI 与服务层经真实 `ak_ffi.dll` 验证（监听事件、取消路径、
  版本往返）；双机真实传输的端到端 UX 验证待后续波次。
- **身份卡占位**：ak-ffi 尚未导出身份查询（无 `ak_identity_info`），故
  `deviceInfo()` 返回：deviceName = 真实主机名、指纹/配对码 = 空串（主页指纹
  组件显示"指纹无效"、配对码区为空——诚实占位不误导）。建议的 ak-ffi 后续
  API：`ak_identity_info(engine, id_dir, buf, cap)` 返回
  `{"fingerprint":"<64hex>","pairing_code":"123-456"}`，落地后本层即插即用。
- **配对簿（peers.book）未接入**：接收页"配对簿（默认）"准入会拒绝启动并
  提示改用"指定指纹/配对码"或确认后 any（与页面文案承诺一致）；发送侧指纹
  仅接受完整 64 位十六进制，前缀需配对簿补全故暂拒绝。
- **任务历史为空**：`listJobs()` 本波固定返回空，待任务检查点存储
  （docs/BACKLOG.zh-CN.md P1-03）接入。
- **多文件选择对话框/二维码**：本波未集成（接收页有对应说明文案）。
