# M0 原生平台实验

仅用合成数据在本机建立真实 UDP/TLS 会话，证明链接和标量 FFI 调用路径。不是文件客户端，不读用户文件，不保存密钥，不连接公网或第三方服务器。自动批准只存在于这个本机自测 fixture，不能移植为产品接收策略。

原生代码在独立 Cargo workspace 中，生产 core/transport 的依赖不变。先 `cargo fetch --manifest-path experiments/platform_probe/native/Cargo.toml --locked`，再执行 `python scripts/run_native_validation.py --output validation/native-attempt`。首次编译需设置目标平台 linker/SDK；CI 保存配置、真实日志、退出码、产物大小与摘要。

ABI 见 `include/ak_m0_probe.h`。start 返回正整数句柄或负错误；最多一个任务，完成也必须 release。poll/cancel/release 不等待网络，release 在工作线程未退出时返回 busy。取消和成功以首次终态 CAS 为界；不能取消一个已完成的任务。加载库不自动运行；必须在显式按钮/测试中启动。调用方卸载库之前必须取消并完成 release，否则工作线程可能仍在执行库代码。

版本固定见 `config/toolchains.json`（仓库根目录）。最低系统是待测试目标，不是已支持声明。Windows Server CI 不等于 Windows 11 真机；iOS 原生程序链接不等于 App 签名、安装或运行；Android ELF 16KiB 对齐不等于真机验证。Flutter 最小壳在后续独立模块中接入。

资源测量模板中的空值是未测，不能当作零。原生库体积/本机实验耗时不是 APK/IPA 下载大小、手机 PSS 或应用冷启动。无公网 Wi-Fi、目录持久权限、后台与进程终止须使用真实设备，缺失时保持 BLOCKED。
