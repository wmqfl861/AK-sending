# 当前交接：M0-06/07工程已审核，完整M0待实机验收

## 基线与分支

仓库wmqfl861/AK-sending。当前工作分支 `dev/m0-07-device-lan-harness-20261102`，草稿PR #8；M0-06在草稿PR #7。最终受测且独立复审代码 `253cc6c601253170e6847f59aef3c8bbd0c03d5f`，树 `5133b67485df850a996e77cd40d877e76f27eef1`。本交接随后的提交只改文档、证据索引和MANIFEST，下次必须读取最新HEAD，不能把该实现SHA当永久HEAD。

先行计划：M0-06 `4bf8fa66b9863c5ca2baa498c8521c37fa84f42d`，端点排空补充 `531a4e442a227618b1798e0aebeb3b800d27a01d`；M0-07 `b533fe2969067a58cbed2511ec8aab0e0b8825b0`。保持 [AGENTS](../AGENTS.md) 的先计划→开发→测试→审核→修复复测→交接。未合并、部署、采购、强推或操作生产签名。

## 本轮实际交付

M0-06：独立两端临时配对、9个有界标量LAN ABI、冻结对端/角色、一次性本机合成许可、真实UDP/TLS两固定帧、取消/过期/release；CLI及三桌面独立进程测试、Linux无默认路由隔离栈。生产新增消耗式有界shutdown排空，不更改TLS/授权/依赖。

M0-07：五端LAN页面与纯Dart控制器、严格公开offer、完整指纹核对/勾选/明确启动、异步权限代次和后台/销毁保护、原平台权限及用途声明；Apple新增导出保留校验。增加设备操作手册、未填模板和只验证一致性的证据检查器，不产生虚假真机记录。

首轮Flutter41567457463的组件测试错误已修复：有界滚动定位懒构建按钮，并新增完整确认流程，保留未确认不得start断言；不是跳过失败。最终独立回执6594175099明确审查253cc6c601、未发现重大问题。M0-06回执6593782051针对310079dbf9；不是正式APPROVED或专项安全审计。

## 最终测试与核验

core41568269666、native41568269744、Flutter41568269677、mobile41568269667均成功。三桌面每系统110核心Rust、16native Rust；Linux/macOS61 Python，Windows58通过+3项Unix专用跳过；五个Flutter主机任务各20 Dart；两个Apple主机各6 Swift真实文件回归。三桌面实际Dart FFI新旧入口通过，LAN网络通过独立进程/隔离栈，不冒称手机UI网络实测。

13份最终artifact均下载，核对ZIP、精确commit、所有命令退出码和原始日志/产品摘要；五份source.zip各140个正规化Git blob匹配受测树。详细索引见[M0-07_VALIDATION](../reports/M0-07_VALIDATION.json)，[审核](../reports/M0-07_REVIEW.zh-CN.md)。本地Rust/Dart工具经只读公开工具包核验后使用；本地执行Rust/native/Python及纯Dart检查，完整Flutter来自CI。本地netns权限不足，隔离栈成功来自CI，不绕过限制。

M0-06历史两份报告从PR #7文档提交733513116e3cd15146c9b81d8ede542bf371af1b原样继承，保留其受测SHA，不把历史结果改写为新结果。当前新增功能的正负例和复审绑定本轮最终SHA。

## 下一节点与明确边界

**LAN独立驱动和五端操作入口已经实现，不需要再重复开发这两个旧缺口。完整M0仍PARTIAL，阻断在实际设备退出验收。** 跟踪[Issue #9](https://github.com/wmqfl861/AK-sending/issues/9)，按[设备操作手册](../experiments/flutter_probe/LAN_DEVICE_RUNBOOK.zh-CN.md)和[设备矩阵](../experiments/flutter_probe/DEVICE_MATRIX.zh-CN.md)取得真实手机/无线网络/权限/目录/进程/最低系统与资源证据。

Android为测试签名APK，iOS为未签名App ZIP，后者需本地可信Xcode签名安装且记录实际安装产物。当前没有接入可操作物理Android/iOS，不得用构建、namespace、组件fake或人工填表代替实测。证据检查器退出0只是单记录完整性，不是M0通过。

实验仅私网IPv4、临时证书、固定合成内容；不是生产密钥管理、文件引擎或可靠保存。手机切后台/系统弹窗会取消，恢复不自动重启，配对数据交换需可信带外方式并保持实验页面；这不是最终配对UX。平台系统调用可能不立即响应取消，保持busy而非无限叠任务。

M1文件I/O/校验/续传、M2公网NAT仍需另行先行计划；不能提前实现它们来掩盖M0未验收。新发现实机问题先写小模块修复计划。历史失败和未测项不删。

## 产物与回退

五端极小实验产物约Windows11.90、Linux10.23、macOS9.95、Android19.39、iOS8.41 MiB（分别ZIP或APK），不是完整产品预算结论。CI artifact保留14天，文本日志另保全；过期不保证远端ZIP可取。

回退按模块移除实验页面/绑定/工具，保留认证/授权/无中继约束；不改main。保持单写入者，远端发生新提交先核对。无会话结束后自动继续的承诺。
