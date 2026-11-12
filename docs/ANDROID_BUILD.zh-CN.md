# Android 交叉编译指南（ak-ffi）

本文档说明如何在 Windows（Git Bash）上把 Rust 侧 `ak-ffi`（cdylib）交叉编译为
Android 的 `libak_ffi.so`，并放入 Flutter 工程供 `ak_ffi_dart` 加载。一站式脚本：
`scripts/build_android_ffi.sh`。

> 标记约定（见 AGENTS.md）：本文只描述**构建流程**。`llvm-nm` 符号验证属于静态产物
> 检查，不等价于真机运行验证；真机/模拟器运行验证另行记录。

## 前置条件

### 目录布局（本仓库约定的安装位置）

```
%USERPROFILE%\android-toolchain\
├── jdk\                        # JDK 17（flutter / sdkmanager 需要）
├── dl\                         #（可选）安装包缓存
└── sdk\                        # Android SDK 根
    ├── cmdline-tools\latest\bin\
    ├── platform-tools\
    └── ndk\
        └── <版本号>\            # 例如 27.2.12479018；脚本自动取最高版本
```

- SDK 根默认取 `$USERPROFILE/android-toolchain/sdk`，可用 `--sdk` 或环境变量
  `ANDROID_SDK_ROOT` 覆盖。
- NDK 默认在 `<sdk>/ndk/<ver>` 中自动发现（取版本号最高者），可用 `--ndk` 或
  `ANDROID_NDK_HOME` / `NDK_HOME` 覆盖。
- 安装 NDK 示例：
  `sdkmanager --sdk_root="$USERPROFILE/android-toolchain/sdk" --install "ndk;27.2.12479018"`
  （首次需先接受许可：`sdkmanager --licenses`，或 `flutter doctor --android-licenses`）。

### 工具链要求

- Rust（rustc ≥ 本仓库现行版本）+ 两个 Android target：
  `rustup target add x86_64-linux-android aarch64-linux-android`
- Git Bash（脚本只用 bash + coreutils：ls/sort/grep/sed/cp/mkdir/stat/sha256sum）。
- NDK r23+（工具链为 `toolchains/llvm/prebuilt/windows-x86_64/bin` 下的
  `<triple><api>-clang.cmd` 包装脚本与 `llvm-ar.exe`）。

## 方案取舍：环境变量 vs `.cargo/config.toml`

`.cargo/config.toml` 的 `[target.'cfg(...)']` linker 配置必须写死 NDK 完整路径，
而 NDK 落地目录含动态版本号（`sdk/ndk/<ver>`），静态配置会随版本升级失效，还会
和其他并行工作在仓库根的 cargo 配置互相覆盖。因此本仓库采用**纯环境变量方案**：

- `CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER` / `CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER`
  → NDK 的 `x86_64-linux-android24-clang.cmd` / `aarch64-linux-android24-clang.cmd`
  （`.cmd` 后缀在 Windows 上 cargo 直接可用，这是 Rust 官方 Android 文档推荐写法）。
- `CC_x86_64_linux_android` / `CC_aarch64_linux_android` → 同名 `.cmd`
  （`ak-store` 的 bundled SQLite 由 cc crate 编译，需要按目标指定 CC）。
- `AR_x86_64_linux_android` / `AR_aarch64_linux_android` → NDK 的 `llvm-ar.exe`。

这些变量只在脚本进程内生效，不污染仓库配置；**仓库不新增 `.cargo/config.toml`**。

## 脚本用法

```bash
cd AK-sending
bash scripts/build_android_ffi.sh            # 默认：SDK=~/android-toolchain/sdk，API 24
bash scripts/build_android_ffi.sh --ndk ~/android-toolchain/sdk/ndk/27.2.12479018
bash scripts/build_android_ffi.sh --api 24 --sdk /c/OtherSdk
```

一次运行完成：定位 NDK → 导出上述环境变量 → `cargo build -p ak-ffi --release
--target x86_64-linux-android --target aarch64-linux-android` → 复制产物 →
符号验证 → 打印大小/SHA-256。任一前置缺失时脚本给出预期路径并退出码 1。

### 产物与 ABI 对应

| Rust target（triple）         | Android ABI | 用途             | jniLibs 落位                                |
|-------------------------------|-------------|------------------|---------------------------------------------|
| `x86_64-linux-android`        | `x86_64`    | 模拟器（x64）    | `flutter_app/android/app/src/main/jniLibs/x86_64/libak_ffi.so`   |
| `aarch64-linux-android`       | `arm64-v8a` | 真机（arm64-v8a）| `flutter_app/android/app/src/main/jniLibs/arm64-v8a/libak_ffi.so` |

中间产物在 `target/<triple>/release/libak_ffi.so`；脚本复制到 jniLibs 并校验
两处 SHA-256 一致。Flutter/Gradle 打包时 `src/main/jniLibs/<abi>/` 会自动进 APK。

## 符号验证

脚本内置验证（冻结 ABI 的抽查，完整契约见 `crates/ak-ffi/src/shim.rs` 与 crate
header）：用 NDK 的 `llvm-nm -D --defined-only` 检查两个 `.so` 均导出

- `ak_engine_new`
- `ak_identity_info`
- `ak_jobs_list`

手工复查：

```bash
NM="$USERPROFILE/android-toolchain/sdk/ndk/$(ls "$USERPROFILE/android-toolchain/sdk/ndk" | sort -V | tail -1)/toolchains/llvm/prebuilt/windows-x86_64/bin/llvm-nm.exe"
"$NM" -D --defined-only flutter_app/android/app/src/main/jniLibs/arm64-v8a/libak_ffi.so | grep -E "ak_engine_new|ak_identity_info|ak_jobs_list"
```

## 故障排查

| 症状 | 原因与处理 |
|------|-----------|
| `ERROR: Android SDK not found` | `~/android-toolchain/sdk` 不存在：等安装完成，或 `--sdk`/`ANDROID_SDK_ROOT` 指向实际 SDK。 |
| `ERROR: no NDK found under the SDK` | `<sdk>/ndk/` 无任何版本目录：`sdkmanager --install "ndk;<版本>"`，或 `--ndk` 直指 NDK 目录。 |
| `NDK tool not found: ...clang.cmd` | NDK 不完整或 API 级别无对应包装脚本：换 `--api 21`，或重装完整 NDK。 |
| `linker 'x86_64-linux-android24-clang.cmd' not found`（cargo 报错） | 显式传了坏的 `CARGO_TARGET_*_LINKER` 或 NDK 被移动：重新运行脚本让其重新定位。 |
| `cc` crate 报 `compiler not found` / sqlite3.c 编译失败 | 环境变量被外部覆盖：确认脚本内导出的 `CC_*_linux_android` 指向 NDK `.cmd` 且未被外层 shell 覆盖。 |
| `rustup target add` 网络失败 | 先在有网环境执行 `rustup target add x86_64-linux-android aarch64-linux-android`（一次性）。 |
| `sdkmanager` 报 license 未接受 | `sdkmanager --licenses` 或 `flutter doctor --android-licenses` 全部接受后重试。 |
| 构建成功但应用加载失败 | 确认真机为 arm64（`adb shell getprop ro.product.cpu.abi`），模拟器为 x86_64；ABI 不匹配时系统不会加载错误目录的 `.so`。 |

## 已知限制

- 仅覆盖 `x86_64` 与 `arm64-v8a` 两 ABI；`armeabi-v7a`/`x86`（32 位）未配置、未验证。
- 产物未做 size class 优化（无 `strip`/`optimize-for-size`）；如 APK 体积敏感可后续
  追加 `strip = true` 或手工 `llvm-strip`。
- Windows 主机走 `windows-x86_64` 的 NDK；脚本同时探测 `linux-x86_64` 以便同一脚本
  在 Linux CI 复用，但后者未在本机验证。
- `MANIFEST.sha256` 已按 E4 决定删除（2026-11-12）；`flutter_app/android/` 工程目录由集成者维护，脚本只创建
  `jniLibs/<abi>/` 子目录。
