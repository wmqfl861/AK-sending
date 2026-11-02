# 存储、原件与平台适配

## 保存目标

桌面默认用户 Downloads/AKTransfer，用户可改；Android 首次选择系统允许的共享目录并持久化 URI 授权；iOS 默认应用 Documents 收件目录，支持 Files 选择器授权其他目录。[S6][S7]
默认目标与单次目标分开；不通过普通文件路径操作 content URI。不申请系统未授予的任意目录写入。
SaveTarget 保存位置标识、授权信息、读写/寻址/重命名能力和可用性，不保存访问密钥到协调服务器。
目录撤权、磁盘移除、书签失效、第三方提供器不可用时暂停，保留可恢复的本地状态。
选中云盘目录时提示系统可能另行同步；此行为不是 AK互传的服务器传输路径，不可作为绕过 ADR 的默认策略。

## 写入与提交

优先直接写入目标目录的临时文件，完成后安全提交。提供器不能随机写或原子提交时明确降级方案，提前计算可能的额外暂存空间。
剩余空间未知时允许实际写入错误处理，不以虚假剩余容量放行。空间不足不误报成功。
文件名规范化、重名映射、大小写差异、非法字符及超长路径均在接收端处理并保留映射报告。
目录遍历拒绝绝对路径、..、路径分隔注入；针对符号链接/硬链接与 TOCTOU 另做系统 API 防护，字符串检查不足以保证安全。
首版不默认跟随符号链接，不跨出用户选择的源目录。空目录必须保留。

## 原件

相册导入需取得原始资源，不依赖 UI 缩略图。明确区分原件、编辑后版本、Live Photo 关联资源。
禁止自动 HDR/HEVC 兼容转码；跨端不能播放不等于文件损坏。只在云端的资源需先由系统下载，离线时给出明确原因。
具体 Apple/Android 导入和后台 API 按 P0 固定的系统版本重新核查，不能把旧版插件默认行为当规格。

## 本机数据模型

Device(device_id, public_key, display_name, capabilities)、TrustGrant(subject, device, scope, expires_at, revoked)、TransferJob(job_id, endpoints, state, save_target, source_version)、FileEntry(file_id, relative_path, size_u64, digest, status)、ChunkState(file_id, verified_ranges, checkpoint_version)、SaveTarget(platform_handle, capabilities, permission_state)。
SQLite 是本机状态，不存传输文件内容；超大清单分页，禁止全部读入内存。u64 数据要明确定义超出 SQLite 有符号整数范围时的存储编码，避免溢出。
数据迁移及损坏恢复需要测试，不把数据库内容当未经验证的落盘证明。

## 平台发布矩阵（待 P0 冻结）

Windows、macOS、Linux、Android、iOS 全部为目标，不代表当前已完成。
Linux 先限定发行版/桌面环境/包格式；macOS 区分 ARM64 与 Intel 的当前工具链支持；移动端固定最低版本和 ABI。
本地发现权限、防火墙、后台执行、通知、系统共享、签名与更新均须原生验证。
手机锁屏/后台/被系统终止/用户强制退出分开验证；不承诺全情形常驻。P0 立即做 iPhone 真机验证，不能推迟到桌面完成后。
