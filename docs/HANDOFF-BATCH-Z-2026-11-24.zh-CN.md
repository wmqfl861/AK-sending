# 交接文档：批次 Z（2026-11-24）

> 起点：批次 X 分支 `dev/batch-x-20261122` 的收尾提交 `5b6ae31`（codex 第十五轮批准之后的那个提交）。批次 X 尚未合入 main，本批叠在该分支之上，分支 `dev/batch-z-20261124`；批次 X 合入之后，本批分支跟着上去。
>
> - 批次 X 的结论与证据见 [X 交接](HANDOFF-BATCH-X-2026-11-22.zh-CN.md)；X 交接 §3.2 把「批次 Y」留给了 PAKE 短码，所以本批记作 Z；
> - 文案规范见[全项目审核 §4.5](AUDIT_2026-11-22.zh-CN.md#45-文案规范)；
> - 每项状态只写在[审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)；
> - 执行方从本文 §8 证据表开始记录本批的运行编号与计数。

## 0. 一句话现状

所有者 2026-11-24 答复了批次 X 留下的三个问题：K1 按工作单补做（已补齐，codex 批准，见 X 交接 §8）；**Android 文件夹发送按 codex 推荐的方法实现**，需要手机测试时由执行方给所有者安装包；审核循环按 codex 建议收口（已收口）。本批只做第二件：让 Android 应用能把一个文件夹连同子目录、空子目录、零字节文件原样发出去。

## 1. 要解决的问题

X4 的发送路径要求「可多选，含文件夹」，但 Android 上文件夹发不出去：系统选择器给的是 `content://` 树形授权（SAF），发送引擎只收文件系统路径，应用又没有读共享存储的权限。`flutter_app/lib/ui/service/file_pick_gateway.dart` 在这种情形下拿到的是不可发送的伪路径，被 P1-H 的过滤规则当成「取消」处理。X 交接 §8 把它记为已知能力缺口，X4 的「含文件夹」因此没有任何端到端证据。

## 2. 做法

### 2.1 数据怎么走

发送侧取得所选文件夹的树形读授权，经 `ContentResolver` 枚举并读取内容，复制到应用私有目录（暂存），保留根目录名、相对结构、空子目录和零字节文件，然后把**暂存根目录的路径**交给现有发送引擎。清单、分块、校验、续传全部沿用；FFI、Dart 绑定、线上协议、`docs/contracts/ffi-v2.md` 都不改，不加权限，不加依赖。

暂存的代价是发送前多一次完整读写，占用约等于文件夹总量的内部存储；所以分两段做（先枚举与估算，再复制），带进度、可取消，空间不足时明确失败并清理。

### 2.2 为什么是这条路

codex 计划比较了三种做法，结论是方案 a：

| 做法 | 结论 |
|---|---|
| a. SAF 读取后暂存，再走现有路径引擎 | 改动限于 Android SAF、Dart 选取与暂存生命周期、界面状态；引擎与协议不动。**采用** |
| b. 引擎增加可插拔读取源（文件描述符或回调） | 要改引擎的清单、分块读取、源稳定性校验、续传与 FFI 契约，实现与验证面大得多。不采用 |
| c. 新增共享存储权限或走 MediaStore | 违反红线，也表达不了任意目录树。不采用 |

### 2.3 执行方对计划的修订与 codex 的两轮确认

执行方冻结接口前核对了源码，对 codex 的初版计划做了一处修订：**不改 `pickDirectory()`，新增 `pickSendDirectory()`**——设置页用 `pickDirectory()` 选接收保存目录（`settings_page.dart`），照原计划改会把接收目录变成暂存副本的路径。发送侧的三个入口改调新方法：`pickFiles()` 的「添加文件夹」与「只发送一个文件夹」两条路径，以及详情页的「选择文件夹」。

codex 第一轮确认（§8 有行）认可修订方向，并指出必须先改的地方，执行方逐条落实（都在 §3 里）：

- 下载目录的 provider 是 `com.android.providers.downloads.documents`，执行方原先写错；
- `implements FilePickGateway` 的类不继承接口上的方法体，六处假网关必须各补齐新方法；
- 暂存根与完成标记会重名，改为 `ak-saf-staging/<id>/payload/<根名>`，标记写在 `<id>` 下、最后创建；
- 核对所选 URI 本身的持久化读授权，暂存结束即撤销，进程中断遗留的只读授权由清理识别并撤销；
- 取消必须等原生停手并删完暂存目录才应答；监听先于暂存调用安装；
- 枚举要有条目上限与溢出检查，大小未知的文件不计入估算，实际字节数单独报告；
- 完全空的文件夹单独发送时清单为空（多根追加时引擎会为空目录补一项），本批统一在暂存时拒绝并提示；空子目录与零字节文件保留；
- 名称规则按引擎的 `ak_core::manifest::validate_rel_path`（`crates/ak-core/src/manifest.rs`），控制字符按 Unicode 字符检查，校验范围含根名、完整子路径和加根名前缀后的路径；
- 删除不跟随符号链接；暂存根下的一切变更串行在一个工作线程；
- 页面侧的归属要补全：首页销毁、交接回调失败；详情页发送成功后释放并把条目移出已选列表，失败的发送不因为 Future 返回就释放。

codex 第二轮确认（§8 有行）又指出五处必须在冻结前改的地方，执行方在四路实现进行中把更正写回契约并逐路送达：

- **接收端布局**：单独发送一个文件夹时，接收端把文件夹的**内容**直接放进目标目录（`<目标>/nested/…`），没有「根名」这一层；只有追加发送（多根）才给每条路径加根名前缀。执行方原先写反了，照原样比对会把正确的传输判成失败。依据：`scan_tree` 跳过根自身、`SourceSet::open` 单根时原样使用清单、接收端直接以目标目录落盘；
- **读授权只撤销发送侧自己新增的那一份**：同一个目录可能正是接收保存目录（持有读写两位的持久授权），无条件撤销读权限会削掉接收所需的权限。所以 `pickSendFolder()` 不再持久化任何授权，`stageSafTree()` 开始时先看所选目录是否已有读授权，没有才取、结束时才撤销；
- **删除先做不跟随的判型**（`Os.lstat`）：指向另一个暂存任务目录的符号链接也能通过总根的包含检查，`isDirectory()` 会跟随它；
- **名称**：原始显示名含 `/` 的先按单个组件拒绝（引擎会把 `a/b` 读成合法的两层路径）；等于 `.ak-staging`（不分大小写）的组件在暂存时拒绝——接收端为它自己的暂存目录保留了这个名字；
- **未确认的取消要隔离**：`sendFiles` 超时且取消原生任务失败时，后台可能仍在读文件；这次发送快照里的暂存条目之后不能被清空、重选或离页删掉，页面只移除引用，副本交给以后的过期清理。

另有两处提醒也落实了：详情页 `_pickSource` 在 `await` 之后发现页面已销毁时要释放刚拿到的条目；可用空间要在与暂存目录同卷的 `noBackupFilesDir` 上查，有大小未知的文件时估算只是下限、进度保持不定。

## 3. 冻结的接口

### 3.1 原生通道 `dev.ak/storage`

| 方法 | 结果 | 说明 |
|---|---|---|
| `pickSendFolder()` | `{status:"selected", treeUri, displayName}` 或 `{status:"cancelled"}` | 不持久化任何授权，只用选择器给的临时授权读根目录名；来源不在允许清单（外部存储与下载目录两个 provider）时报 `SAF_PROVIDER_UNTRUSTED` |
| `stageSafTree(treeUri, stageId)` | `{status:"ok", stageId, path, displayName, fileCount, byteCount, emptyDirectoryCount}` | 后台线程；进度事件 `stagingProgress`（阶段、已见条目、已复制文件数与字节数、估算总量）走同一通道；开始时所选目录若没有持久读授权才取、结束时才撤销（已是接收保存目录的读写授权不动）；应答时原生已完全停手 |
| `cancelStaging(stageId)` | null | 只置标志，由进行中的暂存自己清理后应答 `STAGE_CANCELLED` |
| `cleanupStagedSend(stageId)` | null | 幂等；只删 ID 格式合法的本应用暂存目录 |
| `cleanupStaleStaging()` | `{removed}` | 应用启动时调一次：删 ID 非法的项、没有完成标记的项、完成超过 24 小时的项，并撤销遗留的只读授权 |

错误一律是 `PlatformException(code, message)`，`message` 是固定的原因记号，不带 URI、路径或名称。错误码：`SAF_PERMISSION_DENIED`、`SAF_PROVIDER_UNTRUSTED`、`SAF_ENUMERATION_FAILED`、`SAF_UNSUPPORTED_DOCUMENT`、`STAGE_NAME_CONFLICT`、`STAGE_NO_SPACE`、`STAGE_IO_FAILED`、`STAGE_CANCELLED`、`STAGE_INVALID_ID`。

### 3.2 Dart 网关

`PickedEntry` 增加三个可选字段：`stagingId`、`stagedFileCount`、`stagedByteCount`（只有暂存过的文件夹非空）。`FilePickGateway` 增加两个抽象方法：`pickSendDirectory()`（发送侧单文件夹，返回条目或 null）与 `releaseStaged(entries)`（把暂存副本交还，尽力而为、幂等、不抛错）。

生产网关在 Android 上把选目录、暂存、进度、取消、错误与重试都收在 `pickSendDirectory()` 里（经根 navigator 弹窗，与 X4 的两个文件夹 offer 同一做法），对页面只返回「条目或 null」。错误弹窗标题「无法发送此文件夹」，按钮「重新选择」「取消」。编译期开关 `kAndroidFolderSendEnabled` 关闭时，Android 不弹文件夹 offer，发送文件夹只提示暂不支持，绝不把选择器的伪路径交给引擎。非 Android 平台委托回 `pickDirectory()`，桌面行为不变。

### 3.3 暂存条目的归属

暂存副本归当前持有该 `PickedEntry` 的一方，归属结束时调用 `releaseStaged`：

- 首页：流程结束、选择被替换、因流程代际变化而丢弃的结果、流程在「更改」打开期间结束、页面销毁时仍归首页所有的条目、post-frame 交接回调发现页面已销毁。交接给详情页是转移，不是释放。
- 详情页：发送成功（同时把这些条目移出已选列表，避免再发一次已释放的路径）、清空、被新的选择替换、没有发送在进行时页面销毁。发送失败不自行释放：页面仍在则留给重试，页面已销毁则交给过期清理——Future 返回不等于引擎已停止读取。
- 两个容易漏的口子：详情页 `_pickSource` 在 `await` 之后发现页面已销毁时，释放刚拿到的条目；发送超时且**取消原生任务失败**（服务层抛出的 `TimeoutException`，文案含「取消原生任务失败」）时，后台可能仍在读，这次发送快照里的暂存条目只从已选列表移除、不经任何后续路径释放，副本交给以后的过期清理。

## 4. 批次 Z 工作项

| 项 | 步骤 | 验收 |
|---|---|---|
| Z0 | 立项：从 X 分支头切出 `dev/batch-z-20261124`；codex 计划与确认入 §8；接口冻结（§3）；接口骨架——`PickedEntry` 三个可选字段、`FilePickGateway` 两个新方法、六处 `implements` 实现补齐、`pickFiles()` 两条文件夹路径改经 `pickSendDirectory()` | 骨架上 `flutter analyze` 与 `flutter test` 通过 |
| Z1 | 原生：`SafSendStager.kt` 与 `MainActivity.kt` 的五个方法、进度事件、错误表（§3.1） | Kotlin 编译通过；模拟器 profile 探针通过（Z4） |
| Z2 | Dart 网关：`pickSendDirectory()` 的 Android 分支、进度与取消、错误与重试、`releaseStaged()`、启动清理、追加路径里与已选文件重名的拒绝、开关（§3.2） | 单元测试覆盖 §3.2 的每一条 |
| Z3 | 页面：详情页「选择文件夹」改调 `pickSendDirectory()`；暂存条目在各归属结束点释放（§3.3）；widget 测试 | 既有测试全过；每个释放点有一条测试 |
| Z4 | 模拟器 profile 探针：真实系统树形选择器 → 原生暂存 → 逐项核对相对路径、大小、SHA-256 → 释放；另经原始通道做失败路径探针（非法 ID、来源不在清单、复制中取消、重复释放、清理不触碰进行中的任务）；再验一次「已是接收保存目录的读写授权在发送之后仍在」 | 证据行入 §8；真实 provider 上构造不出的分支写「未验证」 |
| Z5 | 模拟器端到端走查：经真实发送页把暂存文件夹发给宿主接收端，核对相对路径、空子目录、零字节文件与逐个文件的 SHA-256（单独发送时接收端直接得到文件夹的内容） | 证据行入 §8 |
| Z6 | 安装包：arm64 profile 构建，指向 154 测试服务器的测试注册中心与 STUN；`apksigner verify --print-certs`；记 APK 的 SHA-256；交所有者真机测试（§6.2） | 真机证据由所有者提供后入 §8；没有则写「待所有者」 |
| Z7 | 全部门槛、CI、codex 审核；状态表 Z 行与 STATUS | 全部门槛通过；裁决行入 §8 |

四路并行：Z1（原生）、Z2（网关）、Z3（页面）、Z4 与 Z5 的测试与驱动脚本同时进行，文件所有权互不重叠；集成后由执行方统一跑门槛、探针、走查与安装包。

**不在范围**：桌面端的任何行为变化；接收侧；引擎与协议；云盘等非本地 provider（不在允许清单，明确拒绝并提示改选本地文件夹）；完全空的文件夹（拒绝并提示）；按条目上限必须拆分发送的超大文件夹。

留给后续批次（codex 第二轮确认时列出的）：大目录的流式读取；暂存的跨进程续作；扩展元数据的保真；用可控的 provider 为原生侧在真实设备上构造不出来的异常分支（重复 document ID、虚拟文档、查询或读取异常）补证。

## 5. 门槛与命令

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --no-fail-fast
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
cargo test --locked -p ak-store --features sqlite
cargo test --locked -p ak-signal --features server-persistence
python -m unittest discover -s scripts/tests -v
python scripts/check_repository.py
python scripts/check_contracts.py
bash -n scripts/deploy_linux.sh && bash scripts/deploy_linux.sh --self-test
bash scripts/cross_nat_test.sh --self-test
cd flutter_app && flutter analyze && flutter test && cd ..
cd flutter_app/packages/ak_ffi_dart && dart test && cd ../../..
cd flutter_app/android && ./gradlew :app:compileDebugKotlin --console=plain -q && cd ../..
cd flutter_app && flutter build apk --profile --target-platform android-arm64 \
  --dart-define=AK_REGISTRY_ADDR=<测试注册中心> \
  --dart-define=AK_REGISTRY_ID=<测试注册中心标识> \
  --dart-define=AK_STUN_ADDR=<测试 STUN> && cd ..
```

补充说明：

- 本批不改 Rust；Rust 各项照样跑并记录，不因为「没改」省略；
- 安装包的三个 `--dart-define` 必须全部显式给出：注册中心没有编译期默认值，STUN 的默认值指向生产服务器；
- profile 构建沿用现有的 debug 签名配置，不生成签名密钥；
- 模拟器上的结果与所有者真机上的结果分别记录，模拟器结果不写成真机验证。

## 6. 测试服务器与真机测试

### 6.1 测试服务器

所有者允许把测试服务器排跑在 154 测试服务器（地址不入库；它不是生产服务器）。执行方在那台机器上做了这些事，都是所有者账号下的用户级操作：

- 用仓库源码构建了 Linux 版 `ak-cli`（release，对应提交 271b1c1）；
- 起了两个 systemd 用户服务：测试注册中心（TLS，TCP 7200）与 STUN（UDP 7101）。两者是临时单元，机器重启后需要重新起；
- **没有改那台机器的防火墙**，没有登录或修改生产服务器。

验证结果见 §8：两个端口从开发机公网可达；接收端在同一台机器上自发自收通过；从开发机跨公网发送时配对成功、文件数据通路没有连通。

### 6.2 所有者真机测试

需要所有者做的事只有安装和操作手机，接收端由执行方准备。建议按这个顺序：

1. 执行方交安装包（arm64、profile 构建、debug 签名）和一个接收码。接收端优先放在与手机同一局域网的电脑上——这样文件数据走局域网直连，不受公网 NAT 影响；公网直连单独记录。
2. 手机上准备一个测试文件夹（执行方提供内容与清单，或自己建一个）：里面要有子目录、一个空子目录、一个零字节文件和一个较大的文件。
3. 打开应用，走「对方已经给了你一个码？输入对方的码」的发送入口，粘贴接收码并配对。
4. 进入连接详情，点「选择文件夹」，在系统选择器里选中测试文件夹并点「允许」。应用会显示「正在准备文件夹」的进度。
5. 进度结束后点发送，等界面显示「发送完成」。
6. 告诉执行方已发完。执行方在接收端逐项核对相对路径、大小和 SHA-256，并确认暂存目录已经清理。

公网直连的边界（按 codex 的措辞）：154 测试机上的注册中心与 STUN 可以从公网访问；一次从开发机发起的公网文件发送没有连通，原因没有单独隔离——开发机处在多层 NAT 之后，接收端的文件数据端口是临时 UDP 端口，测试机防火墙目前只放行注册与 STUN 端口。所以那次失败不能单独归因于某一种 NAT，也不能推论同类网络都必然失败。应用遵守无中继设计，不会经测试服务器传文件。

## 7. 红线

沿用 X 交接 §7 全部各条，另加本批的说明：

- 不加第三方依赖，测试依赖也不加（Android 原生侧因此没有单元测试，见 §8 的未验证项）；
- 不改清单权限：暂存目录在应用私有存储内；
- 154 测试服务器是所有者允许的测试环境，不是生产服务器；执行方不改它的防火墙，它的地址不入库；
- 暂存后的内容只是本次发送的快照：哈希比对证明文件内容一致，不承诺保留文件系统时间戳等元数据。

## 8. 证据表（执行方填写）

执行方在本机 Windows 与 Android 模拟器上工作。按 Z0–Z7 的顺序逐行填写；每行写明环境、提交或运行编号、原样输出与结论。四路并行子代理的结果先由执行方复核再入表。

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
| 批次 Z 执行计划（codex gpt-6.2-astra/max，项目规则首轮应用） | 本地 codex 会话（2026-11-24 出具，全程只读，656,001 tokens，出具时 HEAD 为 2b36d43） | 计划确认方法为方案 a（SAF 读取后暂存，再走现有路径引擎），并给出三种做法的比较与实施清单。认可执行方对初版计划的修订（新增 `pickSendDirectory()`，不改 `pickDirectory()`；错误、进度、取消与重试收在生产网关；两段式暂存；名称规则镜像 `ak_core::manifest::validate_rel_path`）。指出必须先改的地方：下载目录的 provider 是 `com.android.providers.downloads.documents`；`implements FilePickGateway` 的类不继承接口上的方法体；暂存根与完成标记重名；读授权的核对、撤销与中断后的清理；取消必须等原生停手；枚举的条目上限与溢出；完全空的根目录；单根与多根的路径语义；删除不得跟随符号链接；页面侧归属的各处遗漏；发送成功后不得再发已释放的路径。列出合并前的 P1 十条、P2 三条，验证分层与所有者真机步骤。**计划确认：需修改**。计划原文在会话记录，未入库 |
| 批次 Z 计划第二轮确认（codex） | 本地 codex 会话（2026-11-24 出具，全程只读，306,453 tokens，出具时 HEAD 为 238ab9c） | 确认上一轮的修订方向都已到位；指出五处必须在冻结前改的契约条款：单根发送的接收布局（接收端直接得到文件夹的内容，没有根名一层——执行方原先写反）、读授权只能撤销发送侧自己新增的那一份（同一目录可能正是接收保存目录）、删除先用 `Os.lstat` 做不跟随的判型、原始名称含 `/` 先按单个组件拒绝且 `.ak-staging`（不分大小写）在暂存时拒绝、未确认的原生取消要隔离这次发送快照里的暂存条目；另指出探针任务书的同步项（文件数、暂存总根的位置、监听先于暂存、ACTIVE 用例的事后检查）、两点提醒（`_pickSource` 在页面销毁后释放刚拿到的条目；可用空间在同卷的 `noBackupFilesDir` 上查）与留给后续批次的 P2。**计划确认：需修改**。执行方把五处修订写回契约并逐路送达正在工作的四路实现，**没有再送第三轮确认**——更正逐条来自本轮意见，由结束审核把关 |
| Z0 分支与接口骨架 | 本地 Windows；分支 `dev/batch-z-20261124`，自 `5b6ae31` 切出；骨架提交 238ab9c | `PickedEntry` 三个可选字段；`FilePickGateway` 两个抽象方法（`pickSendDirectory()`、`releaseStaged()`）；六处 `implements FilePickGateway` 的实现各补齐这两个方法（`test/w28_test_helpers.dart`、`test/home_flow_test.dart` 与四个集成测试文件）；`pickFiles()` 的两条文件夹路径改经 `pickSendDirectory()`；`kAndroidFolderSendEnabled` 常量与空的静态 `cleanupStaleStaging()`。骨架不改行为（`pickSendDirectory()` 暂时委托回 `pickDirectory()`）。骨架上 `flutter analyze`：`No issues found!`；`flutter test`：`All tests passed!`（360 项） |
| 154 测试服务器上的测试服务（所有者允许；不是生产服务器） | 154 测试服务器（Ubuntu 24.04，多项目共用主机；地址不入库）；systemd 用户级的临时单元 `ak-test-registry`、`ak-test-stun`、`ak-test-recv` | 用仓库源码在该机构建 Linux 版 `ak-cli`（release；`ak-cli --version`：`wire v1, caps 0x0017 [resume\|pause-resume\|multistream\|sender-keepalive], release build, git 271b1c1`）。测试注册中心监听 TLS 7200（标识前 8 位 `88b5758e`），STUN 监听 UDP 7101；两台服务都设了失败自动重启，机器重启后需要重新起。**公网可达**（从开发机验证）：TCP 7200 可连接；向 UDP 7101 发一个 STUN Binding 请求，收到 `0x0101` 应答且 magic cookie 正确。**同机自发自收**：接收端与发送端都在该机上，经测试注册中心配对，一个 4 MiB 的文件，收发两端 SHA-256 相等（`036d0eea…`）。**从开发机跨公网发送**：任务码兑换与配对成功，文件数据通路没有连通——发送端对接收端全部候选地址的探测无应答，QUIC 握手超时；接收端的日志显示它向发送端的候选地址打了洞。**没有改这台机器的防火墙**，没有登录或修改生产服务器。归因的边界见 §6.1 |
| Z1 原生暂存器（子代理执行，执行方复核） | 提交 6f2bdc6；`flutter_app/android/app/src/main/kotlin/dev/ak/ak_sending_app/SafSendStager.kt`（新）与 `MainActivity.kt` | `SafSendStager.kt` 约 1150 行：五个通道方法、`stagingProgress` 事件、两段式（`enumerateTree` → `copyPlan`）、lstat 判型的删除、授权的取与放、名称规则；`MainActivity.kt` 注册第二个选择器启动器与五个 `when` 分支，`onDestroy` 取消在跑的暂存。**编译**：`cd flutter_app/android && ./gradlew :app:compileDebugKotlin --console=plain`（需要设 `JAVA_HOME` 与 `ANDROID_HOME`）→ `BUILD SUCCESSFUL`，唯一的告警是未改动代码里插件弃用的六条提示；执行方独立重编一次确认。子代理另在宿主 JVM 上用真实源码跑了纯逻辑自检 134 项全过（名称规则对照 `crates/ak-core/src/manifest.rs` 的测试向量、ID 规则、1024 字节规则、授权失败与 ENOSPC 的分类、只应答一次的保护）——该自检在仓库外，没有入库。执行方复核了授权、删除、名称与复制四段，没有发现问题；目标路径一律从规范化路径拼出（Android 上 `/data/user/0` 本身就是符号链接）。**子代理自述的取舍与未做项**：单个分量超过 255 字节没有预检（复制期以 `STAGE_IO_FAILED`/`containment` 失败）；EDQUOT 没有按空间不足处理；取消只在块、条目与目录之间生效，不打断阻塞中的 provider 调用；选择时的根目录名查询与 `stageSafTree` 里的授权调用在主线程（前者是一次本地 provider 查询）；没有 Kotlin 单元测试（不加测试依赖）。设备上的行为见 Z4 行 |
| Z2 Dart 网关（子代理执行） | 提交 bb39078；`flutter_app/lib/ui/service/file_pick_gateway.dart`、新 `saf_send_dialogs.dart`、新 `test/file_pick_gateway_saf_test.dart` | `pickSendDirectory()` 的 Android 分支：同一时刻一次选取、首次暂存前等待启动清理、选取与暂存的循环、进度对话框与取消、失败后「无法发送此文件夹」对话框与「重新选择」；只关闭自己压入的进度路由；`releaseStaged()`、每进程一次的 `cleanupStaleStaging()`；追加路径里与已选文件同名的暂存文件夹被拒；开关关闭时 Android 不弹两个文件夹 offer。`pickDirectory()`、`_isFileSystemPath`、两个 offer 对话框与 HEAD 逐字相同。通道替身的单元测试 51 项。新增冻结键十个：`staging_progress_dialog`、`staging_progress_text`、`staging_cancel_button`、`staging_error_dialog`、`staging_error_title`、`staging_error_body`、`staging_error_retry_button`、`staging_error_cancel_button`、`folder_send_unsupported_body`、`folder_send_unsupported_ok_button`。**最终文案**：进度「正在准备文件夹」，枚举时「正在读取文件夹内容…」，复制时「已复制 N 个文件 · X」，点了取消后「正在取消…」，按钮「取消」；开关关闭时「暂时无法发送文件夹」「这个版本还不支持直接发送文件夹，请只选择文件。」，按钮「关闭」；失败时「无法发送此文件夹」，按钮「取消」「重新选择」，正文按错误码与原因记号共十一句（读权限、位置不支持、条目太多、无法完整读取、空文件夹、无法原样发送、名称不能原样发送、接收方保留的名称、与已选文件同名、空间不足、准备失败）。**取舍**：取消输掉竞争时（复制已经完成）副本被释放、选取答 null；暂存结果里的路径不是文件系统路径（树形 URI、`msd:`、`/storage/msd/…`）或形状未知时按拒绝处理并释放副本；非 `PlatformException` 的异常不吞 |
| Z3 页面的暂存归属（子代理执行） | 提交 8e9947b；`home_page.dart`、`connection_detail_page.dart`、`app.dart`、`test/w28_test_helpers.dart`、`test/connections_test.dart` 与四个新测试文件 | 详情页「选择文件夹」改调 `pickSendDirectory()`。释放点——首页七处（流程结束、更改替换、初次选取因代际变化被丢弃、更改结果因代际变化被丢弃、流程在更改打开期间结束、`dispose`、post-frame 交接回调发现页面已销毁）；详情页六处（发送成功并移出已选列表、清空、被新的选择替换、没有发送在进行时销毁、`await` 之后发现页面已销毁、未确认的原生取消——只移除不释放）。另有 `_send` 防重入，以及发送进行中落地的选取不释放引擎仍在读的那份副本。新 widget 测试 37 项；子代理在仓库外的副本里做了变异检查，27 个预期有测试守住的变异全部被发现。**被改的既有断言两条**：`home_flow_test.dart` 的 P1-H（第五、六轮）——Android 发送侧不再走插件的目录选择（`directoryPicks` 由 1 改为 0），`/storage/msd/` 的排除改在 `pickDirectory()` 上断言；`connections_test.dart` 的「选择文件夹」用例——`pickDirectoryCalls == 1` 改为 `pickSendDirectoryCalls == 1` 且 `pickDirectoryCalls == 0`。**取舍**：启动清理的调用放在 `AkSendingApp` 的 State 里；`_settleSelectionChange` 里「流程已结束」那一处释放今天不可达（流程结束总会先抬高代际），没有测试能覆盖它 |
| Z4 模拟器 profile 探针——**通过** | 模拟器（Android 15 API 35，x86_64；`flutter drive --profile`）；最终一轮 2026-11-25 01:16 到 01:21；原始文件留在本机 `target/device-evidence/probe-20261125-0121/`（不入库） | 就绪门三拍的 1 分钟负载 1.53、1.25、1.17，桌面在前台（5 分钟与 15 分钟的均值在 3.4 到 4.2 之间——宿主当时在跑门槛）。`All tests passed!`，`DRIVE-EXIT=0`，`TREE_MATCH (8 manifest lines)`。**A 段（生产网关，真实系统树形选择器）**：前台是 `com.android.documentsui/com.android.documentsui.picker.PickActivity`，依次点中 `akstage`、`Use this folder`、`Allow`；`ZSAF\|A\|STAGED\|name=akstage\|files=4\|bytes=33554461\|staged=true\|pathIsFs=true\|threw=no`；八个条目（`empty.txt` 0 字节、`emptydir`、`nested`、`nested/deep`、`nested/deep/note.txt`、`nested/photo.bin` 33554432 字节、`sub`、`sub/说明 文本.txt`）的相对路径、大小与 SHA-256 和宿主清单逐项相等；`ZSAF\|A\|CHECK\|ok`；释放后 `stageDirGone=true\|stagingRootChildren=0`。**B 段（原始通道）**：`STAGE_INVALID_ID`、`SAF_PROVIDER_UNTRUSTED`、复制阶段取消 `code=STAGE_CANCELLED\|events=2\|lastPhase=copy\|copyEvents=1\|cancelSentAtEvent=2\|bytesAtCancel=0`、重复释放 `first=ok\|second=ok`、清理不触碰进行中的任务 `staleRemoved=0\|staged=ok\|payloadKept=true\|markerKept=true`、结束后只读授权 `left=0`。**C 段**：同一目录先经 `pickFolder` 取得读写授权，再暂存一次，`before=rw\|after=rw`。**首轮（00:23）同样通过**，只有一行信息项 `A\|ROOT\|derivedOk=false`：交叉核对拿 `/data/user/0/…` 的写法去比暂存器应答的规范化路径 `/data/data/…`（同一个目录，Android 上前者是指向后者的符号链接）；核对改为按解析后的路径比较，重跑即 `true`。**未验证**（真实 provider 上构造不出来，又没有原生单元测试）：重复 document ID、虚拟文档、查询或读取异常、条目上限、溢出、空间不足、名称规则在设备上的拒绝、持久化条数到上限、Activity 在暂存中途销毁、机器重启后的残留与 24 小时规则。模拟器，不是物理设备 |
| Z5 模拟器端到端走查——**通过** | 同一台模拟器（profile 构建）；本机注册服务器与 STUN，宿主 CLI `task code-recv` 作接收端；最终一轮 2026-11-25 01:08 到 01:13；原始文件留在本机 `target/device-evidence/e2e-20261125-0113/`（不入库） | 就绪门三拍的 1 分钟负载 1.55、1.37、1.04。载荷：`nested/photo.bin` 4 MiB、两个文本文件、一个零字节文件与一个空目录。`ZSAF\|E2E\|CODE_ENTERED\|path=controller-callback` → `JOIN_SUBMITTED\|send-entry\|****XB45` → `PICKER` → `STAGED\|picked=1\|name=akstage\|rootChildren=1\|payloadOk=true` → `RESULT\|发送完成：已发送 8 个文件（4.0 MB） · 互联网直连 / 用时 40 秒 · 平均 101 KB/s\|stats=用时 40 秒 · 平均 101 KB/s` → `STAGING_ROOT\|children=0` → `DONE`；`All tests passed!`，`DRIVE-EXIT=0`。系统选择器在前台的记录与 Z4 的 A 段相同。宿主：`[recv] final ack verified=true`、`[task] received 8 file(s), 4.00 MiB`；输出目录的顶层直接是 `empty.txt`、`emptydir/`、`nested/`、`sub/`（没有 `akstage/` 一层），`RECV_MATCH (8 manifest lines)`。**此前三次都没有通过，原因都在走查测试自己**：第一次（00:34）暂存成功之后，等待辅助函数在一个不等待的 `pump` 还挂着时去轮询受保护的测试接口，框架报 `Guarded function conflict`，发送没有点出去；第二次（00:45）用了 32 MiB 的载荷，接收端已确认 31 MiB 时 4 分钟的结果等待到期，测试收尾关闭应用，接收端才报对端断开；第三次（00:59）产品链路已经完整走通（`发送完成…用时 43 秒`、`RECV_MATCH`），但最后一个断言拿字符串 `'0'` 去比整数 `0`。修法：就绪条件只用查找器的 `evaluate()`、等待结束前确保没有挂着的 `pump`、载荷改为 4 MiB、结果等待放宽到 7 分钟、断言改回整数。**观察（不在本批范围）**：完成文案写「已发送 8 个文件」，而这个文件夹是 4 个文件加 4 个目录——引擎把目录条目也计了数。模拟器，不是物理设备；速度是这台模拟器的数字 |
| 本批的过程记录（如实） | 编排脚本在仓库外，不入库 | 1) 四路并行子代理在 2026-11-24 22:11 被账号的使用上限打断，22:23 从中断处恢复；中断时工作区仍可编译（`dart analyze` 无告警）。2) 模拟器进程在 21:28 之后丢失（原因没有查明），22:27 重新冷启动。3) 网关那一路的子代理在仓库外的副本里做变异检查时留下了一个孤儿测试进程，执行方按 CPU 时间与进程树识别后终止；仓库里的测试并不挂起。4) 探针与走查的宿主脚本由子代理编写，并在仓库外做了一个不碰设备的自检（58 项）。5) 四项设备验证之间产品代码没有改动（三个实现提交都早于第一次探针）；第一次端到端走查失败之后，执行方只改了走查测试与宿主脚本（见 Z5 行） |
| Z6 安装包（交所有者真机测试） | 由提交 9c4a42a 构建：`cd flutter_app && flutter build apk --profile --target-platform android-arm64 --dart-define=AK_REGISTRY_ADDR=<测试注册中心> --dart-define=AK_REGISTRY_ID=<测试注册中心标识> --dart-define=AK_STUN_ADDR=<测试 STUN>`（三个值都指向 154 测试服务器，不入库） | `√ Built build\app\outputs\flutter-apk\app-profile.apk (50.8MB)`，Gradle 任务 82.4 秒。`apksigner verify --verbose --print-certs`：`Verifies`，v2 签名，`Signer #1 certificate DN: C=US, O=Android, CN=Android Debug`——沿用本机的 debug 签名，没有生成密钥。APK 的 SHA-256 `2be44a8b…`，53258151 字节。`lib/arm64-v8a/` 有全部六个库；`lib/x86_64/` 只有工程自带的三个预编译库、没有 Flutter 引擎，所以这个包装不到 x86 设备上。`libapp.so` 里能检到测试地址两处，也能检到生产 STUN 地址一处——后者是没传 `AK_STUN_ADDR` 时的编译期默认值（`_defaultStunAddr`），本包传入了测试 STUN，运行时取不到它。安装包、合成测试文件夹、它的清单与压缩包放在所有者桌面的一个文件夹里（路径见回复）。**真机证据：待所有者**（步骤见 §6.2）；接收端已在本机起好，走局域网直连，配对经 154 的测试注册中心 |
| Z7 本机门槛（实现提交 9c4a42a 上；本批不改 Rust） | 本地 Windows | `flutter analyze`：`No issues found!`；`flutter test`：`All tests passed!`（448 项，骨架时是 360 项）；`cd flutter_app/packages/ak_ffi_dart && dart test`：`All tests passed!`（172 项）；`python -m unittest discover -s scripts/tests`：`Ran 382 tests`、`OK`（宿主负载高，耗时 2624 秒）；`python scripts/check_contracts.py`：`RESULT: 5/5 checks PASS`；`cargo fmt --all -- --check` 通过；`cargo doc --locked --workspace --no-deps`（带 `RUSTDOCFLAGS="-D warnings"`）通过；clippy、cargo test 与仓库检查见下一行 |
| Z7 本机门槛（续）与仓库检查 | 本地 Windows；代码为提交 9c4a42a，仓库检查在文档提交上跑 | `cargo clippy --locked --workspace --all-targets -- -D warnings`：`Finished`（退出码 0）；`cargo test --locked --workspace --no-fail-fast`：69 个套件、1238 项通过、0 失败、11 项忽略（退出码 0）；`cargo test --locked -p ak-store --features sqlite`：`54 passed; 0 failed`；`cargo test --locked -p ak-signal --features server-persistence`：`115 passed; 0 failed`；`bash -n scripts/deploy_linux.sh && bash scripts/deploy_linux.sh --self-test`：`[self-test] PASS (nothing was contacted, nothing was deployed)`；`bash scripts/cross_nat_test.sh --self-test`：`PASS: self-test (no server contacted, no SSH, no transfer)`；`python scripts/check_repository.py`：`errors: []`。`cargo audit` 与 `cargo deny` 本机没有单独跑，由 CI 承担，结果记在 Z-CI 行 |
