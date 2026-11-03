# M0-07：五端独立LAN操作入口开发后审核

日期2026-11-02（America/Los_Angeles；CI UTC为11月3日）。**工程子范围REVIEWED_DEVICE_LAN_HARNESS_ONLY；模块总验收与整个M0仍为PARTIAL_PHYSICAL_DEVICE_GATES_NOT_RUN。**

## 提交与范围

先行计划 `b533fe2969067a58cbed2511ec8aab0e0b8825b0`；首实现 `8b464c9601eff3a67ca1a758bc7bfe4e68067075`；最终受测且独立复审 `253cc6c601253170e6847f59aef3c8bbd0c03d5f`，树 `5133b67485df850a996e77cd40d877e76f27eef1`。计划先于实现独立提交。PR #8草稿，不合并；后续报告提交不冒充被审代码SHA。

新增LAN页面、纯Dart控制器、9个标量FFI绑定、有界公开offer解析、完整证书摘要核对、两端显式合成许可、权限回调代次/后台取消/释放。新增Android既有权限处理入口、iOS system-mediated短状态和macOS用途声明；Apple保留并核验全部14个原生符号。原生产核心、M0-06 native实现、依赖和锁文件未因本模块改变。

新增设备操作手册、NOT_RUN/null模板、单设备记录完整性检查器及负例。检查器不证明物理硬件、操作者诚实或整体M0批准；超过预算的值保留，不会伪装成零。

## 计划验收映射

| 项目 | 结果及边界 |
|---|---|
| A1 回归/状态 | 三桌面各110核心Rust、16native Rust；Linux/macOS61项Python，Windows58通过+3项Unix专用跳过；五个Flutter任务各20项Dart主机测试 |
| A2 构建/接口 | 五端release通过；三桌面实际Dart调用新LAN ABI生成临时身份、读取公开摘要并取消释放；旧Dart loopback网络测试保留；新LAN网络通过独立进程和隔离栈验证，不称手机UI已运行 |
| A3 操作入口 | 新页面与CLI支持两个独立端点、手动地址、公开配对信息和完整摘要核对、接收端先监听；不是只有127.0.0.1按钮 |
| A4 证据工具 | 12项Python用例覆盖完整记录、缺项/未测/错误SHA/模拟器/目录越界/符号链接/哈希/空文件/资源样本/时区/重复键；fixture不是真机证据 |
| A5 审核 | 独立复审与最终受测SHA一致；未发现重大问题；代码工程范围通过，真机部分仍未执行 |

用户未勾选核对或未明确点击开始时不调用native start；配置与准备不等于网络授权。权限回调在取消、后台、销毁或新代次后失效。拒绝、busy、异常不继续；iOS system-mediated明确不是granted。当前实验任何非resumed状态均取消，恢复前台不自动重启，系统弹窗之后可能需要用户重新准备。

## 失败、修复与复审

首轮Flutter CI `41567457463`：Linux实际18项通过、1项组件测试失败，原因是ensureVisible访问ListView尚未创建的远处按钮。修复改为指定外层Scrollable、有上限的scrollUntilVisible，保留“未导入/未确认不得启动”断言，并新增完整导入→确认→启动→dispose取消路径；没有删除、跳过测试或弱化权限。最终20项通过。生产代码未因此修改。

本地模板回归曾因新增macOS Info.plist用途声明而缺少fixture文件失败，已补真实模板fixture再提交；历史本地日志保留。没有用静态源码存在替代Flutter编译。

独立执行者chatgpt-codex-connector[bot]（199175422）。首轮8b464复审无重大问题，但CI仍失败，因此当时未标完成。修正后的[回执6594175099](https://github.com/wmqfl861/AK-sending/pull/8#issuecomment-6594175099)明确审查253cc6c601；汇总6594045561为completed、无未解决发现。不是以请求评论或表情推断通过，也不是正式APPROVED或安全专项审计。

## 最终真实验证

- Core CI **41568269666**：三桌面各110项Rust、fmt/clippy/feature/source-clean通过；Python按上表记录跳过情况。
- Native CI **41568269744**：三桌面各16项Rust、真实ctypes、独立进程3组正负例；Linux两个无默认路由的隔离网络栈完成正常、错证书、取消；Android ELF/16KiB/新增定义导出、iOS原生引用链接通过。
- Flutter CI **41568269677**：Windows/Linux x64、macOS/Android/iOS arm64全部release成功；每任务20项Dart主机测试，三桌面真实Dart FFI；两个Apple主机各6项Swift实际文件回归，非iOS系统权限验收。
- Mobile core CI **41568269667**：两个移动目标库编译检查成功，不是设备运行。

13份最终artifact全部实际下载，核验ZIP、results精确SHA、每项命令退出码、每份原始日志摘要和原生/应用产物摘要；五份source.zip按提交的Git属性正规化后各140个blob与受测树一致。Windows工作树换行不同不当作改源码。所有工程验证基于已提交锁文件与固定SDK，runner本身不是位级固定。

本地本轮实际执行110项核心Rust、16项native Rust、61项Python、Rust fmt/clippy、Dart格式/纯模块分析和真实Dart LAN ABI烟测；完整Flutter组件/五端构建来自上述CI，不冒称本地装有完整Flutter。物理设备工具未接入；本地namespace权限不足，成功证据来自CI，不绕过权限。

| 实验产物 | 字节 | MiB | 口径 |
|---|---:|---:|---|
| Windows x64 | 12,480,916 | 11.90 | release目录ZIP |
| Linux x64 | 10,723,606 | 10.23 | release目录ZIP |
| macOS arm64 | 10,435,331 | 9.95 | release App ZIP |
| Android arm64 | 20,335,269 | 19.39 | 测试密钥签名APK |
| iOS arm64 | 8,816,142 | 8.41 | 未签名App ZIP |

这些是极小实验程序，不是完整产品、商店下载量、安装占用或手机内存。iOS未签名ZIP不能直接安装为IPA，macOS不声明Intel支持。没有新增运行时依赖。

## 剩余风险和M0交接

当前代码已提供独立两端LAN原生与五端UI入口，不再保留“LAN驱动尚未实现”这一旧缺口。但是本轮没有操作物理Android/iOS，也没有真实双设备Wi-Fi、系统权限拒绝/撤销、目录持久授权、后台/锁屏/杀进程、最低系统或资源预算数据；不得把组件fake、库链接或namespace计入这些项目。

实验仅手动私网IPv4，完整摘要需操作者在可信另一端实际核对；临时身份不替代生产配对密钥生命周期。活动任务期间切到其他App交换信息会取消当前实验，操作时需保持两端页面并用可信带外方式交换，当前不是成熟配对UX。系统调用不一定立即响应取消，保留busy与有界reaper，不承诺强制杀线程。设备记录一致性不是硬件真实性证明，未测模板必须继续拒绝。

[实机阻断Issue #9](https://github.com/wmqfl861/AK-sending/issues/9)、[操作手册](../experiments/flutter_probe/LAN_DEVICE_RUNBOOK.zh-CN.md)、[M0总表](M0_STATUS.zh-CN.md)、[机器证据](M0-07_VALIDATION.json)。用户文件流式I/O、校验/可靠保存/续传归M1，公网NAT产品归M2；本轮未混入这些范围。

回退仅M0-07页面/绑定/平台用途声明/证据工具，保留M0-06认证LAN模块和全部历史审核。artifact保留14天，交付包另保全原始文本日志。未合并、部署、采购、强推或操作生产签名。
