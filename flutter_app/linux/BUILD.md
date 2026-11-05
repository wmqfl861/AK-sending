# Linux 平台构建说明（flutter_app/linux/）

> 第二十四次波（2026-11-05）由 `flutter create --platforms=linux --org dev.ak .`
> 生成的 Flutter Linux runner（与 `windows/` 平级；`lib/`、`test/`、`windows/`
> 均未改动）。本文记录 Linux 构建的前置条件、CMake 配置、`libak_ffi.so`
> 部署方式与已知限制。构建产物契约见 [../README.md](../README.md)
> "DLL 三级探测"，FFI ABI 见 [../../docs/contracts/ffi-v2.md](../../docs/contracts/ffi-v2.md)。

## 1. 前置条件（需要在 Linux 主机上）

Flutter 的 Linux 桌面构建**只在 Linux 主机上执行**（见第 5 节已知限制第 1 条）。

- Flutter stable（含 Linux desktop 设备支持：`flutter doctor` 应显示
  "Linux toolchain - develop for Linux desktop"）；
- 编译工具链：`clang`、`cmake`、`ninja-build`；
- GTK3 开发库：`pkg-config` + `gtk+-3.0`（runner 链接
  `PkgConfig::GTK`，见 `runner/CMakeLists.txt`）；
- Rust 工具链（生成 `libak_ffi.so`）：
  `cargo build -p ak-ffi --release`，产物在
  `<仓库>/target/release/libak_ffi.so`；
- 交叉编译（可选，在非 Linux 主机上仅能到 `cargo check`）：
  `rustup target add x86_64-unknown-linux-gnu`，且需要目标 C 编译器
  （如 `x86_64-linux-gnu-gcc`）供 `ring`/`blake3`/`libsqlite3-sys`
  的构建脚本使用，见 [../../scripts/check_targets.sh](../../scripts/check_targets.sh)
  头部说明。

## 2. CMake 配置

- 顶层 `CMakeLists.txt`：`BINARY_NAME=ak_sending_app`，
  `APPLICATION_ID=dev.ak.ak_sending_app`；`CMAKE_INSTALL_RPATH="$ORIGIN/lib"`；
- 可运行产物是 **install bundle**（`build/linux/x64/release/bundle/`）：
  可执行文件在 bundle 根目录，Flutter 引擎库与插件在 `bundle/lib/`，
  资源在 `bundle/data/`（构建树里的 `intermediates_do_not_run/` 不可直接运行）；
- `AK_FFI_SO` 缓存变量可显式指定 `.so` 路径：
  `cmake -DAK_FFI_SO=/path/to/libak_ffi.so ...`。

## 3. libak_ffi.so 部署

顶层 `CMakeLists.txt` 中的 `--- ak-ffi ---` 段是
[../windows/runner/CMakeLists.txt](../windows/runner/CMakeLists.txt)
DLL 拷贝模式的 Linux 对应实现（解析顺序一致，先到先得）：

1. CMake 缓存变量 `AK_FFI_SO`；
2. 环境变量 `AK_FFI_SO`，其次 `AK_FFI_DLL`（与文档化的 tier-1 覆盖同名）；
3. `<仓库>/target/release/libak_ffi.so`（Linux 主机原生
   `cargo build -p ak-ffi --release`）；
4. `<仓库>/target/x86_64-unknown-linux-gnu/release/libak_ffi.so`
   （交叉构建产物）。

命中后 `install(FILES ...)` 把 `libak_ffi.so` 安装到 **bundle 根目录
（可执行文件同目录）**；未命中只给 CMake WARNING，不阻断构建
（应用会以"无传输引擎"状态启动）。放在 exe 同目录而不是 `lib/`，
是因为 [../lib/services/dll_path.dart](../lib/services/dll_path.dart)
的 tier 2 探测打开的是
`<Platform.resolvedExecutable 所在目录>/libak_ffi.so` 全路径
（Dart 侧不走 `$ORIGIN/lib` rpath）。`defaultAkDllFileName()`
在 Linux 上返回 `libak_ffi.so`（已确认，本次未改动该文件）。

Linux 主机上的完整构建：

```bash
cargo build -p ak-ffi --release          # 产出 target/release/libak_ffi.so
flutter build linux --release            # 产出 build/linux/x64/release/bundle/
```

## 4. 交叉可移植性检查（本机可跑）

[../../scripts/check_linux_targets.sh](../../scripts/check_linux_targets.sh)
对 FFI 面向的三个 crate 做
`cargo check --target x86_64-unknown-linux-gnu -p ak-ffi -p ak-cli -p ak-session`
（输出格式与 `check_targets.sh` 一致：PASS / BLOCKED / FAIL 分类 + 汇总表）。
本机（Windows）结果为 **BLOCKED**——`libsqlite3-sys` 构建脚本需要目标
C 编译器；作为佐证，无 C 依赖的 `{ak-core, ak-wire}` 子集在该 triple 上
PASS。这是宿主环境缺口，不是代码问题；完整矩阵仍由
`scripts/check_targets.sh` 与 Linux CI 覆盖。

## 5. 已知限制

1. **本机（Windows）无法执行 Linux 构建**：`flutter build linux --release`
   在 Windows 主机被 Flutter 工具直接拒绝（退出码 1，
   `"build linux" only supported on Linux hosts.`）。Linux 构建需要
   Linux 主机或完整交叉工具链，本次仅完成 runner 生成与 CMake 打包逻辑。
2. `scripts/check_linux_targets.sh` 在本机为 BLOCKED（见第 4 节），
   不能当作 Linux 编译通过的证据。
3. 按 AGENTS.md 要求：本次没有在真实 Linux 环境运行/验证过应用，
   不把静态检查（`flutter analyze` 0 issue）说成真机验证。
4. `flutter create` 顺带在 `../analysis_options.yaml` 的 exclude 里追加了一行
   `- linux/**`（与既有 `windows/**`、`android/**` 同一模式）。
5. Linux 上的端到端验证（GUI 启动、配对、传输）仍需后续在 Linux 主机
   按仓库验证门槛补做。
