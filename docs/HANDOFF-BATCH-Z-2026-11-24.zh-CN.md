# 交接文档：批次 Z（2026-11-24）

> 起点：批次 X 分支 `dev/batch-x-20261122` 的收尾提交 `5b6ae31`（codex 第十五轮批准之后的那个提交）。批次 X 尚未合入 main，本批叠在该分支之上，分支 `dev/batch-z-20261124`；批次 X 合入之后，本批分支跟着上去。**与分支规则的偏差（如实记录）**：AGENTS.md 要求批次分支从 main 切出，本批没有这样做——所有者让批次 X 的收尾与 Android 文件夹发送连着做，而批次 X 当时还没合入；这一偏差交复核方在合入时裁定。
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

codex 第二轮确认（§8 有行）的裁决同样是「需修改」，又指出五处必须在冻结前改的地方；执行方在四路实现进行中把更正写回契约并逐路送达：

- **接收端布局**：单独发送一个文件夹时，接收端把文件夹的**内容**直接放进目标目录（`<目标>/nested/…`），没有「根名」这一层；只有追加发送（多根）才给每条路径加根名前缀。执行方原先写反了，照原样比对会把正确的传输判成失败。依据：`scan_tree` 跳过根自身、`SourceSet::open` 单根时原样使用清单、接收端直接以目标目录落盘；
- **读授权只撤销发送侧自己新增的那一份**：同一个目录可能正是接收保存目录（持有读写两位的持久授权），无条件撤销读权限会削掉接收所需的权限。所以 `pickSendFolder()` 不再持久化任何授权，`stageSafTree()` 开始时先看所选目录是否已有读授权，没有才取、结束时才撤销；
- **删除先做不跟随的判型**（`Os.lstat`）：指向另一个暂存任务目录的符号链接也能通过总根的包含检查，`isDirectory()` 会跟随它；
- **名称**：原始显示名含 `/` 的先按单个组件拒绝（引擎会把 `a/b` 读成合法的两层路径）；等于 `.ak-staging`（不分大小写）的组件在暂存时拒绝——接收端为它自己的暂存目录保留了这个名字；
- **未确认的取消要隔离**：`sendFiles` 超时且取消原生任务失败时，后台可能仍在读文件；这次发送快照里的暂存条目之后不能被清空、重选或离页删掉，页面只移除引用，副本交给以后的过期清理。

另有两处提醒也落实了：详情页 `_pickSource` 在 `await` 之后发现页面已销毁时要释放刚拿到的条目；可用空间要在与暂存目录同卷的 `noBackupFilesDir` 上查，有大小未知的文件时估算只是下限、进度保持不定。

两轮的裁决都是「需修改」。执行方把两轮意见逐条落实之后冻结了接口，**没有再送第三轮计划确认**——项目规则要求动手前先有 codex 的计划，没有把第三轮确认规定为门槛；落实是否到位由结束审核把关（结果见 §8）。

## 3. 冻结的接口

### 3.1 原生通道 `dev.ak/storage`

| 方法 | 结果 | 说明 |
|---|---|---|
| `pickSendFolder()` | `{status:"selected", treeUri, displayName}` 或 `{status:"cancelled"}` | 不持久化任何授权，只用选择器给的临时授权读根目录名；来源不在允许清单（外部存储与下载目录两个 provider）时报 `SAF_PROVIDER_UNTRUSTED` |
| `stageSafTree(treeUri, stageId)` | `{status:"ok", stageId, path, displayName, fileCount, byteCount, emptyDirectoryCount}` | 后台线程；进度事件 `stagingProgress`（阶段、已见条目、已复制文件数与字节数、估算总量）走同一通道；开始时所选目录若没有持久读授权才取、结束时才撤销（已是接收保存目录的读写授权不动）；每个文件复制完核对字节数与枚举时的已知长度，全部复制完、写完成标记之前逐个目录复核成员（复核之后源目录再变就发现不了）；应答时原生已完全停手 |
| `cancelStaging(stageId)` | null | 置标志、取消该任务的 `CancellationSignal`、关闭它打开的输入流；由进行中的暂存自己清理后应答——目录删净是 `STAGE_CANCELLED`，有残留是 `STAGE_IO_FAILED`/`leftover` |
| `cleanupStagedSend(stageId)` | `{removed: true}` 或 `{removed: false}` | 幂等；只删 ID 格式合法的本应用暂存目录；该 ID 还在暂存则答 `STAGE_IO_FAILED`/`busy`，什么都不删 |
| `cleanupStaleStaging()` | `{removed}` | 应用启动时调一次：删 ID 非法的项、不在进行中的没有完成标记的项、不属于本进程的已交付目录（按暂存目录里的进程令牌 `.owner` 判断——没有任何传输能活过进程重启，以前进程留下的副本不论新旧都是垃圾；本进程交付的目录它不碰），并撤销遗留的只读授权——只撤销归属日志里记着的那些 |

错误一律是 `PlatformException(code, message)`，`message` 是固定的原因记号，不带 URI、路径或名称。错误码：`SAF_PERMISSION_DENIED`、`SAF_PROVIDER_UNTRUSTED`、`SAF_ENUMERATION_FAILED`、`SAF_UNSUPPORTED_DOCUMENT`、`STAGE_NAME_CONFLICT`、`STAGE_NO_SPACE`、`STAGE_IO_FAILED`、`STAGE_CANCELLED`、`STAGE_INVALID_ID`；`STAGE_IO_FAILED` 的原因记号里，结束审核第一轮之后新增的有 `source-changed`（复制期间目录成员变了）、`size-mismatch`（字节数与已知长度不符）、`leftover`（中止后没删净）、`busy`、`id-in-use`；`SAF_PERMISSION_DENIED` 新增 `grant-lookup`（读不到持久授权表）。

几条边界，都是结束审核第一轮之后补上的：

- **读授权的归属。** `stageSafTree()` 先读持久授权表，读不到就失败（`grant-lookup`），绝不当作「没有授权」；所选目录已经有读位（例如它同时是接收保存目录）就原样使用、绝不触碰；没有才取，取之前先把 URI 记进归属日志（`ak-saf-staging/.send-grants`，不入日志、不入库），释放并复核之后才划掉。所有持久授权操作都在工作线程上串行（发送侧的查、取、撤销，过期清理，以及经原生通道 `pickFolder` 取读写授权的那一次）。撤销按**当时**的权限位判断：只有读位仍在、没有写位才撤销读位；撤销之后立刻复读，如果接收侧恰好在这一瞬间把同一目录升级成读写、读位被削掉了，就把读位取回来。过期清理对归属日志里记着、且不属于任何在跑暂存的 URI 用同一条规则。（生产里接收保存目录的选取走的是第三方插件，它的授权默认是临时的、不持久化，应用也没有传选项——所以今天只有发送侧会写持久授权；上面的规则在 Android 11 及以上让两条路径都安全。**Android 7 到 10 上不是这样**：那里撤销一个权限位会把它从「之后可以持久化的位」里一并去掉，所以发送侧在接收侧的选择器结果之后撤销读位，会让接收侧随后对同一目录的取授权失败——见 §8「执行方的独立复审」那一行。）
- **保真的边界。** 复制完之后逐个目录复核成员、逐个文件核对已知长度，这一刻能检出的变化（成员增减、已知长度不符）都让整次暂存失败；复核之后源目录再变、以及长度与成员都没变而内容变了的文件，都发现不了，按读到的内容复制——暂存不是源目录的原子快照；
- **线程与取消。** 所有 `ContentResolver` 调用都在工作线程（选择器返回后的根目录名查询也在那里）；每个任务一个 `CancellationSignal`，取消时会关掉它打开的输入流。
- **ID 与目录内容。** 复用一个已经交付的 ID 会被拒绝（`id-in-use`），不会删掉旧副本；只有没有完成标记的残留才会先删再建。暂存目录里有三样东西：`payload`、最后才写的完成标记、紧跟着建目录就写的 `.owner`（进程令牌）。`cleanupStagedSend` 在调用那一刻就判定：该 ID 还在暂存就立刻答 `busy`、什么都不排队——排在工作队列后面的清理曾经删掉过刚交付的副本。

### 3.2 Dart 网关

`PickedEntry` 增加三个可选字段：`stagingId`、`stagedFileCount`、`stagedByteCount`（只有暂存过的文件夹非空）。`FilePickGateway` 增加两个抽象方法：`pickSendDirectory()`（发送侧单文件夹，返回条目或 null）与 `releaseStaged(entries)`（把暂存副本交还，尽力而为、幂等、不抛错）。

生产网关在 Android 上把选目录、暂存、进度、取消、错误与重试都收在 `pickSendDirectory()` 里（经根 navigator 弹窗，与 X4 的两个文件夹 offer 同一做法），对页面只返回「条目或 null」。原生应答只有在确实属于本次暂存时才被接受：ID 一致，路径是绝对路径、不含反斜杠、冒号、空段、`.` 或 `..`，并以 `<ID>/payload/<根名>` 结尾，三个计数都是非负整数；否则按拒绝处理并释放那份副本。错误弹窗标题「无法发送此文件夹」，按钮「重新选择」「取消」。编译期开关 `kAndroidFolderSendEnabled` 关闭时，Android 不弹文件夹 offer，发送文件夹只提示暂不支持，绝不把选择器的伪路径交给引擎。非 Android 平台委托回 `pickDirectory()`，桌面行为不变。

### 3.3 暂存条目的归属

暂存副本归当前持有该 `PickedEntry` 的一方，归属结束时调用 `releaseStaged`：

- 首页：流程结束、选择被替换、因流程代际变化而丢弃的结果、流程在「更改」打开期间结束、页面销毁时仍归首页所有的条目、post-frame 交接回调发现页面已销毁。交接给详情页是转移，不是释放。
- 详情页：发送成功（同时把这些条目移出已选列表，避免再发一次已释放的路径）、清空、被新的选择替换、没有发送在进行时页面销毁。发送失败不释放：没有任何错误形态能证明引擎已经停止读取——引擎报告的失败也可能来自操作轮询，「已取消原生发送任务」只说明断开调用成功，Rust 侧的读取在阻塞线程里。所以参与过一次已开始发送的暂存条目，只由一次**成功的发送**释放；任何失败都把它留在已选列表里等待重试并保持保护，之后的清空、重选或离页只把它移出列表、不释放，副本在下次应用启动时清掉。从没参与过已开始发送的条目，照旧由清空、替换与销毁释放。详情页的所有释放都经过同一个入口，引擎可能仍在读取的那份副本永远不会被交还（同一帧里点了「发送」之后再触发旧的「清空」也不会）。首页的 post-frame 交接回调会复核页面、流程代际与连接：失效时不打开详情页，并释放它手里的条目。
- 两个容易漏的口子：详情页 `_pickSource` 在 `await` 之后发现页面已销毁时，释放刚拿到的条目；发送超时且**取消原生任务失败**（服务层抛出的 `TimeoutException`，文案含「取消原生任务失败」）时，后台可能仍在读，这次发送快照里的暂存条目只从已选列表移除、不经任何后续路径释放，副本交给以后的过期清理。

## 4. 批次 Z 工作项

| 项 | 步骤 | 验收 |
|---|---|---|
| Z0 | 立项：从 X 分支头切出 `dev/batch-z-20261124`；codex 计划与两轮意见的落实入 §8；接口冻结（§3）；接口骨架——`PickedEntry` 三个可选字段、`FilePickGateway` 两个新方法、六处 `implements` 实现补齐、`pickFiles()` 两条文件夹路径改经 `pickSendDirectory()` | 骨架上 `flutter analyze` 与 `flutter test` 通过 |
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
- 起了两个 systemd 用户服务：测试注册中心（TLS，TCP 7200）与 STUN（UDP 7101）。最初是临时单元；2026-11-25 凌晨该用户的 systemd 管理进程重启时被清掉（原因不在执行方——日志里没有崩溃记录，是管理进程换了进程号），随后改成持久的单元文件 `~/.config/systemd/user/ak-test-registry.service` 与 `ak-test-stun.service`（已启用，随用户的 `default.target` 启动，管理进程或机器重启后会自动起来）；注册中心的标识没有变。测完之后停掉并移除：`systemctl --user disable --now ak-test-registry ak-test-stun`，再删这两个单元文件；
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
| 154 测试服务器上的测试服务（所有者允许；不是生产服务器） | 154 测试服务器（Ubuntu 24.04，多项目共用主机；地址不入库）；systemd 用户级的临时单元 `ak-test-registry`、`ak-test-stun`、`ak-test-recv` | 用仓库源码在该机构建 Linux 版 `ak-cli`（release；`ak-cli --version`：`wire v1, caps 0x0017 [resume\|pause-resume\|multistream\|sender-keepalive], release build, git 271b1c1`）。测试注册中心监听 TLS 7200（标识前 8 位 `88b5758e`），STUN 监听 UDP 7101；两台服务都设了失败自动重启。它们最初是临时单元：2026-11-25 凌晨因该用户的 systemd 管理进程重启而被清掉（管理进程在本地时间 03:04 前后换了进程号，注册中心日志的最后一条在 02:35），执行方在给所有者重起接收端时发现连接不上，随后改成持久单元文件并启用；恢复后注册中心标识不变（前 8 位仍是 `88b5758e`，与安装包里固定的一致），TCP 7200 与 UDP 7101 重新监听。**公网可达**（从开发机验证）：TCP 7200 可连接；向 UDP 7101 发一个 STUN Binding 请求，收到 `0x0101` 应答且 magic cookie 正确。**同机自发自收**：接收端与发送端都在该机上，经测试注册中心配对，一个 4 MiB 的文件，收发两端 SHA-256 相等（`036d0eea…`）。**从开发机跨公网发送**：任务码兑换与配对成功，文件数据通路没有连通——发送端对接收端全部候选地址的探测无应答，QUIC 握手超时；接收端的日志显示它向发送端的候选地址打了洞。**没有改这台机器的防火墙**，没有登录或修改生产服务器。归因的边界见 §6.1 |
| Z1 原生暂存器（子代理执行，执行方复核） | 提交 6f2bdc6；`flutter_app/android/app/src/main/kotlin/dev/ak/ak_sending_app/SafSendStager.kt`（新）与 `MainActivity.kt` | `SafSendStager.kt` 约 1150 行：五个通道方法、`stagingProgress` 事件、两段式（`enumerateTree` → `copyPlan`）、lstat 判型的删除、授权的取与放、名称规则；`MainActivity.kt` 注册第二个选择器启动器与五个 `when` 分支，`onDestroy` 取消在跑的暂存。**编译**：`cd flutter_app/android && ./gradlew :app:compileDebugKotlin --console=plain`（需要设 `JAVA_HOME` 与 `ANDROID_HOME`）→ `BUILD SUCCESSFUL`，唯一的告警是未改动代码里插件弃用的六条提示；执行方独立重编一次确认。子代理另在宿主 JVM 上用真实源码跑了纯逻辑自检 134 项全过（名称规则对照 `crates/ak-core/src/manifest.rs` 的测试向量、ID 规则、1024 字节规则、授权失败与 ENOSPC 的分类、只应答一次的保护）——该自检在仓库外，没有入库。执行方复核了授权、删除、名称与复制四段，没有发现问题；目标路径一律从规范化路径拼出（Android 上 `/data/user/0` 本身就是符号链接）。**子代理自述的取舍与未做项**：单个分量超过 255 字节没有预检（复制期以 `STAGE_IO_FAILED`/`containment` 失败）；EDQUOT 没有按空间不足处理；取消只在块、条目与目录之间生效，不打断阻塞中的 provider 调用；选择时的根目录名查询与 `stageSafTree` 里的授权调用在主线程（前者是一次本地 provider 查询）；没有 Kotlin 单元测试（不加测试依赖）。设备上的行为见 Z4 行 |
| Z2 Dart 网关（子代理执行） | 提交 bb39078；`flutter_app/lib/ui/service/file_pick_gateway.dart`、新 `saf_send_dialogs.dart`、新 `test/file_pick_gateway_saf_test.dart` | `pickSendDirectory()` 的 Android 分支：同一时刻一次选取、首次暂存前等待启动清理、选取与暂存的循环、进度对话框与取消、失败后「无法发送此文件夹」对话框与「重新选择」；只关闭自己压入的进度路由；`releaseStaged()`、每个 Dart isolate 一次的 `cleanupStaleStaging()`；追加路径里与已选文件同名的暂存文件夹被拒；开关关闭时 Android 不弹两个文件夹 offer。`pickDirectory()`、`_isFileSystemPath`、两个 offer 对话框与 HEAD 逐字相同。通道替身的单元测试 51 项。新增冻结键十个：`staging_progress_dialog`、`staging_progress_text`、`staging_cancel_button`、`staging_error_dialog`、`staging_error_title`、`staging_error_body`、`staging_error_retry_button`、`staging_error_cancel_button`、`folder_send_unsupported_body`、`folder_send_unsupported_ok_button`。**最终文案**：进度「正在准备文件夹」，枚举时「正在读取文件夹内容…」，复制时「已复制 N 个文件 · X」，点了取消后「正在取消…」，按钮「取消」；开关关闭时「暂时无法发送文件夹」「这个版本还不支持直接发送文件夹，请只选择文件。」，按钮「关闭」；失败时「无法发送此文件夹」，按钮「取消」「重新选择」，正文按错误码与原因记号共十一句（读权限、位置不支持、条目太多、无法完整读取、空文件夹、无法原样发送、名称不能原样发送、接收方保留的名称、与已选文件同名、空间不足、准备失败）。**取舍**：取消输掉竞争时（复制已经完成）副本被释放、选取答 null；暂存结果里的路径不是文件系统路径（树形 URI、`msd:`、`/storage/msd/…`）或形状未知时按拒绝处理并释放副本；非 `PlatformException` 的异常不吞 |
| Z3 页面的暂存归属（子代理执行） | 提交 8e9947b；`home_page.dart`、`connection_detail_page.dart`、`app.dart`、`test/w28_test_helpers.dart`、`test/connections_test.dart` 与四个新测试文件 | 详情页「选择文件夹」改调 `pickSendDirectory()`。释放点——首页七处（流程结束、更改替换、初次选取因代际变化被丢弃、更改结果因代际变化被丢弃、流程在更改打开期间结束、`dispose`、post-frame 交接回调发现页面已销毁）；详情页六处（发送成功并移出已选列表、清空、被新的选择替换、没有发送在进行时销毁、`await` 之后发现页面已销毁、未确认的原生取消——只移除不释放）。另有 `_send` 防重入，以及发送进行中落地的选取不释放引擎仍在读的那份副本。新 widget 测试 37 项；子代理在仓库外的副本里做了变异检查，27 个预期有测试守住的变异全部被发现。**被改的既有断言两条**：`home_flow_test.dart` 的 P1-H（第五、六轮）——Android 发送侧不再走插件的目录选择（`directoryPicks` 由 1 改为 0），`/storage/msd/` 的排除改在 `pickDirectory()` 上断言；`connections_test.dart` 的「选择文件夹」用例——`pickDirectoryCalls == 1` 改为 `pickSendDirectoryCalls == 1` 且 `pickDirectoryCalls == 0`。**取舍**：启动清理的调用放在 `AkSendingApp` 的 State 里；`_settleSelectionChange` 里「流程已结束」那一处释放今天不可达（流程结束总会先抬高代际），没有测试能覆盖它 |
| Z4 模拟器 profile 探针——**通过** | 模拟器（Android 15 API 35，x86_64；`flutter drive --profile`）；最终一轮 2026-11-25 01:16 到 01:21；原始文件留在本机 `target/device-evidence/probe-20261125-0121/`（不入库） | 就绪门最后一拍的 1 分钟负载 1.17，桌面在前台（5 分钟与 15 分钟的均值在 3.4 到 4.2 之间——宿主当时在跑门槛）。`All tests passed!`，`DRIVE-EXIT=0`，`TREE_MATCH (8 manifest lines)`。**A 段（生产网关，真实系统树形选择器）**：前台是 `com.android.documentsui/com.android.documentsui.picker.PickActivity`，依次点中 `akstage`、`Use this folder`、`Allow`；`ZSAF\|A\|STAGED\|name=akstage\|files=4\|bytes=33554461\|staged=true\|pathIsFs=true\|threw=no`；八个条目（`empty.txt` 0 字节、`emptydir`、`nested`、`nested/deep`、`nested/deep/note.txt`、`nested/photo.bin` 33554432 字节、`sub`、`sub/说明 文本.txt`）的相对路径、大小与 SHA-256 和宿主清单逐项相等；`ZSAF\|A\|CHECK\|ok`；释放后 `stageDirGone=true\|stagingRootChildren=0`。**B 段（原始通道）**：`STAGE_INVALID_ID`、`SAF_PROVIDER_UNTRUSTED`、复制阶段取消 `code=STAGE_CANCELLED\|events=2\|lastPhase=copy\|copyEvents=1\|cancelSentAtEvent=2\|bytesAtCancel=0`、重复释放 `first=ok\|second=ok`、清理不触碰进行中的任务 `staleRemoved=0\|staged=ok\|payloadKept=true\|markerKept=true`、结束后只读授权 `left=0`。**C 段**：同一目录先经 `pickFolder` 取得读写授权，再暂存一次，`before=rw\|after=rw`。**首轮（00:23）同样通过**，只有一行信息项 `A\|ROOT\|derivedOk=false`：交叉核对拿 `/data/user/0/…` 的写法去比暂存器应答的规范化路径 `/data/data/…`（同一个目录，Android 上前者是指向后者的符号链接）；核对改为按解析后的路径比较，重跑即 `true`。**未验证**（真实 provider 上构造不出来，又没有原生单元测试）：重复 document ID、虚拟文档、查询或读取异常、条目上限、溢出、空间不足、名称规则在设备上的拒绝、持久化条数到上限、Activity 在暂存中途销毁、机器重启后的残留与 24 小时规则。模拟器，不是物理设备 |
| Z5 模拟器端到端走查——**通过** | 同一台模拟器（profile 构建）；本机注册服务器与 STUN，宿主 CLI `task code-recv` 作接收端；最终一轮 2026-11-25 01:08 到 01:13；原始文件留在本机 `target/device-evidence/e2e-20261125-0113/`（不入库） | 就绪门最后一拍的 1 分钟负载 1.04。载荷：`nested/photo.bin` 4 MiB、两个文本文件、一个零字节文件与一个空目录。`ZSAF\|E2E\|CODE_ENTERED\|path=controller-callback` → `JOIN_SUBMITTED\|send-entry\|****XB45` → `PICKER` → `STAGED\|picked=1\|name=akstage\|rootChildren=1\|payloadOk=true` → `RESULT\|发送完成：已发送 8 个文件（4.0 MB） · 互联网直连 / 用时 40 秒 · 平均 101 KB/s\|stats=用时 40 秒 · 平均 101 KB/s` → `STAGING_ROOT\|children=0` → `DONE`；`All tests passed!`，`DRIVE-EXIT=0`。系统选择器在前台的记录与 Z4 的 A 段相同。宿主：`[recv] final ack verified=true`、`[task] received 8 file(s), 4.00 MiB`；输出目录的顶层直接是 `empty.txt`、`emptydir/`、`nested/`、`sub/`（没有 `akstage/` 一层），`RECV_MATCH (8 manifest lines)`。**此前三次都没有通过**（原始文件在 `target/device-evidence/e2e-20261125-0040/`、`-0052/`、`-0105/`）。第一次（00:34 发起）：暂存成功之后测试框架报 `Guarded function conflict`——等待辅助函数在一个不等待的 `pump` 还挂着时去轮询受保护的测试接口；那一轮接收端的输出目录是空的，接收端日志里没有收到清单的记录。第二次（00:43:47 发起）：用了 32 MiB 的载荷，4 分钟的结果等待到期时接收端已确认 31 MiB；测试收尾之后接收端报对端断开。断开与收尾的精确先后、以及 32 MiB 在原预算内能否传完，都没有单独验证——下一轮同时缩小了载荷并放宽了等待。第三次（00:59 发起）：产品链路走通（`发送完成…用时 43 秒`、`RECV_MATCH`），最后一个断言拿字符串 `'0'` 去比整数 `0` 而失败。三处修法：就绪条件只用查找器的 `evaluate()`、等待结束前确保没有挂着的 `pump`、载荷改为 4 MiB、结果等待放宽到 7 分钟、断言改回整数。**最终通过的一轮用的是 4 MiB 的载荷**，32 MiB 的情形见修复波之后的复验行。**观察（不在本批范围）**：完成文案写「已发送 8 个文件」，而这个文件夹是 4 个文件加 4 个目录——引擎把目录条目也计了数。模拟器，不是物理设备；速度是这台模拟器的数字 |
| 本批的过程记录（如实） | 编排脚本在仓库外，不入库 | 1) 四路并行子代理在 2026-11-24 22:11 被账号的使用上限打断，22:23 从中断处恢复；中断时工作区仍可编译（`dart analyze` 无告警）。2) 模拟器进程在 21:28 之后丢失（原因没有查明），22:27 重新冷启动。3) 网关那一路的子代理在仓库外的副本里做变异检查时留下了一个孤儿测试进程，执行方按 CPU 时间与进程树识别后终止；仓库里的测试并不挂起。4) 探针与走查的宿主脚本由子代理编写，并在仓库外做了一个不碰设备的自检（58 项）。5) 第一次探针到最终通过的两轮之间产品代码没有改动（三个实现提交都早于第一次探针）；第一次端到端走查失败之后，执行方只改了走查测试与宿主脚本（见 Z5 行） |
| Z6 安装包（交所有者真机测试） | 由提交 f6d456e 构建（第三轮修复之后的产品代码；`flutter clean` 之后的完整构建）：`cd flutter_app && flutter build apk --profile --target-platform android-arm64 --dart-define=AK_REGISTRY_ADDR=<测试注册中心> --dart-define=AK_REGISTRY_ID=<测试注册中心标识> --dart-define=AK_STUN_ADDR=<测试 STUN>`（三个值都指向 154 测试服务器，不入库） | `√ Built build\app\outputs\flutter-apk\app-profile.apk (41.5MB)`，Gradle 任务 170.5 秒。`apksigner verify --verbose --print-certs`：`Verifies`，v2 签名，`Signer #1 certificate DN: C=US, O=Android, CN=Android Debug`——沿用本机的 debug 签名，没有生成密钥。APK 的 SHA-256 `1df957b2…`，43555346 字节。`lib/arm64-v8a/` 有全部六个库；`lib/x86_64/` 只有工程自带的三个预编译库、没有 Flutter 引擎，所以这个包装不到 x86 设备上。`libapp.so` 里能检到测试地址两处，也能检到生产 STUN 地址一处——后者是没传 `AK_STUN_ADDR` 时的编译期默认值（`_defaultStunAddr`），本包传入了测试 STUN，运行时取不到它。安装包、合成测试文件夹、它的清单与压缩包放在所有者桌面的一个文件夹里（路径见回复）。**真机证据：待所有者**（步骤见 §6.2）；接收端已在本机起好，走局域网直连，配对经 154 的测试注册中心。修复波之前交给所有者的那一个包（由 9c4a42a 构建，SHA-256 `2be44a8b…`，53258151 字节）已从桌面文件夹里换掉——它比现在这个包大，差的是增量重打包留下的填充，不是内容；换掉它是为了让所有者测到的是修复之后的代码；第一轮修复之后的那一个包（由 757e2a8 构建，SHA-256 `d2aa445c…`）也出于同样的理由换成了第三轮修复之后的构建 |
| Z7 本机门槛（实现提交 9c4a42a 上；本批不改 Rust） | 本地 Windows | `flutter analyze`：`No issues found!`；`flutter test`：`All tests passed!`（448 项，骨架时是 360 项）；`cd flutter_app/packages/ak_ffi_dart && dart test`：`All tests passed!`（172 项）；`python -m unittest discover -s scripts/tests`：`Ran 382 tests`、`OK`（宿主负载高，耗时 2624 秒）；`python scripts/check_contracts.py`：`RESULT: 5/5 checks PASS`；`cargo fmt --all -- --check` 通过；`cargo doc --locked --workspace --no-deps`（带 `RUSTDOCFLAGS="-D warnings"`）通过；clippy、cargo test 与仓库检查见下一行 |
| Z7 本机门槛（续）与仓库检查 | 本地 Windows；代码为提交 9c4a42a，仓库检查在文档提交上跑 | `cargo clippy --locked --workspace --all-targets -- -D warnings`：`Finished`（退出码 0）；`cargo test --locked --workspace --no-fail-fast`：69 个套件、1238 项通过、0 失败、11 项忽略（退出码 0）；`cargo test --locked -p ak-store --features sqlite`：`54 passed; 0 failed`；`cargo test --locked -p ak-signal --features server-persistence`：`115 passed; 0 failed`；`bash -n scripts/deploy_linux.sh && bash scripts/deploy_linux.sh --self-test`：`[self-test] PASS (nothing was contacted, nothing was deployed)`；`bash scripts/cross_nat_test.sh --self-test`：`PASS: self-test (no server contacted, no SSH, no transfer)`；`python scripts/check_repository.py`：`errors: []`。`cargo audit` 与 `cargo deny` 本机没有单独跑，由 CI 承担，结果见下一行 |
| Z-CI 首次推送 | GitHub API 核实 | af71048 → run [45450345458](https://github.com/wmqfl861/AK-sending/actions/runs/45450345458) completed success（12 项 success，含 Flutter、Android 构建、`cargo audit` 与 `cargo deny`；2 项手动 release 深测按配置 skipped）。本批没有单独跑 release 深测 |
| 批次 Z 结束审核第一轮（codex） | 本地 codex 会话（2026-11-25 出具，全程只读，549,389 tokens，复审区间 5b6ae31..af71048，结束时 HEAD 为 af71048） | **总裁决：需改**。没有 P0：六个提交共改动 24 个跟踪文件，Rust 引擎、FFI、Dart 绑定、`ffi-v2.md`、线上协议、依赖与锁文件、Gradle 配置、清单都没有差异；新增代码只把 SAF 内容复制到应用私有目录再交给既有发送入口；差异与证据目录里没有 relay、TURN、新增密钥、完整任务码、真实用户路径或截图。确认：Z4、Z5 通过（审核方自己读了原始日志、截图与清单，源、设备、暂存、接收四份清单逐行一致，接收目录顶层直接是文件夹内容）；HEAD 的 CI 成功。**所有者尚未提供真机证据，不单独阻断本批批准。** **P1 四项（合入阻断）**：发送侧可能撤掉接收目录原有的读权限——授权查询出异常时被当成「没有授权」，过期清理撤销只读授权缺少发送侧归属的依据；已知长度的短读可以成为成功副本——逐文件长度没有核对，两段之间目录成员的变化也发现不了；详情页可以删除引擎仍在读取的目录——同一帧里「发送」之后旧按钮的「清空」直接释放整份列表，引擎轮询连续故障时服务层抛 `StateError` 而页面没有隔离；首页取消配对后仍可能自动发送刚选的目录——post-frame 交接只查 `mounted`（这段时序基线已有，本批把文件夹接进了这条路径）。**P2 五项**：取消应答与定点清理不能保证目录已删完，进行中或已交付的 ID 没有被保护；Dart 不核对回包属于本次暂存；两个集成测试有能通过的漏检；原生异常分支与阻塞行为缺少可重复验证（授权与查询调用在主线程，阻塞中的调用不能被取消打断）；STATUS 有互相矛盾的当前结论。**P3**：单个分量超过 255 字节与 EDQUOT；完成文案的口径；「每进程一次」的表述过宽，过期判定对符号链接的缺口；两轮计划的裁决都是「需修改」、没有第三轮确认，分支没有从 main 切出；Z5「原因都在测试自己」的证据不足。审核方的结论：批准前要关闭的是授权误撤、可检测的短读被接受，以及取消或引擎仍在读取时页面继续交付或删除源目录。**执行方处置**：修复波见下一行；P2、P3 里的文档项已改正（STATUS 重写、Z5 行按日志能证明的范围改写并更正发起时间、分支偏差与「没有第三轮确认」如实写明） |
| 修复波（codex 结束审核第一轮的处置） | 提交 d285b86（原生）、66a4048（网关）、2041a87（页面）；集成测试的修订随复验提交 | 四路并行子代理按契约的第三版修订执行，执行方集成。**原生**：读不到持久授权表就让暂存失败（`grant-lookup`），不再当作「没有授权」；新增归属日志 `ak-saf-staging/.send-grants`——取授权之前先记，释放并复核之后才划掉，过期清理只撤销日志里记着的只读授权；每个文件复制完核对已知长度（`size-mismatch`），写完成标记之前逐个目录复核成员（`source-changed`）；`cleanupStagedSend` 答 `{removed: true}` 或 `{removed: false}`，ID 还在跑答 `busy` 且什么都不删，中止后没删净答 `leftover`；已交付的 ID 被复用时拒绝（`id-in-use`）；`ContentResolver` 调用全部移到工作线程，每个任务一个 `CancellationSignal`，取消时关闭打开的输入流；255 字节规则、EDQUOT 归入空间不足、暂存根下不是真实目录的条目按其自身清扫。**网关**：回包必须属于本次暂存（ID 一致、路径形状、计数合法）；新增三句文案；单元测试由 51 项增至 82 项。**页面**：所有释放经过同一个入口，引擎仍在读取的 ID 不会被交还；发送失败除非服务层确认任务已经结束（引擎报告的失败，或文案含「已取消原生发送任务并断开该连接」的超时）一律隔离，`StateError` 与未知类型的错误也在内；首页的交接回调复核页面、流程代际与连接；新增回归测试 9 项。**集成测试**：条目类型比对、取消应答之后目录必须已经消失、推导根与只读授权余数改成硬断言、清理应答的形状；新增 D 段——复制期间目录成员变化、文件被截短，由宿主介入。门槛：`flutter analyze`：`No issues found!`；`flutter test`：`All tests passed!`（488 项，修复前 448 项）；Kotlin 重编 `BUILD SUCCESSFUL`。子代理在仓库外的自检（不入库）：原生的宿主 JVM 纯逻辑检查 235 项全过；页面的变异检查里 31 个有测试守住的变异全部被发现；网关对 11 个故意改坏的变体全部被预期的测试捕获。**取舍与没有关闭的**：页面按结构识别引擎报告的失败（界面层不引入 FFI 包，沿用 `errorCodeOf`）；`dispose` 不再按发送阶段判断，统一由那个入口把住；交接回调里「连接变了而代际没变」的条件没有能单独隔离它的测试；同一帧里的「清空」仍会清空列表并重置发送卡（既有行为，只是不再释放在读的副本）；授权检查移到工作线程之后，一次新的暂存会先取消在跑的那次、再做自己的授权检查 |
| 修复后的设备复验（模拟器，profile 构建）——**通过** | 代码为提交 d285b86、66a4048、2041a87，集成测试为 757e2a8；原始文件留在本机 `target/device-evidence/`（不入库） | **通过的三轮。** (1) **暂存探针**（冷启动并等负载回落之后，2026-11-25 04:46 到 04:54；就绪门 4 拍，最后一拍 1.02）：`All tests passed!`，`TREE_MATCH (8 manifest lines)`，`TREE_RESTORED`；测试在虚拟机服务起来 16 秒后开始。A 段——前台是 DocumentsUI 的选择器；`ZSAF\|A\|STAGED\|name=akstage\|files=4\|bytes=33554461`；八个条目的类型、相对路径、大小与 SHA-256 都和宿主清单一致（类型现在是比较出来的）；释放后 `stageDirGone=true\|exists=true\|children=0\|stageDirs=0\|other=0\|scratch=false\|journal=absent`；`A\|ROOT\|derivedOk=true\|resolved=true`。B 段——`B\|BADID\|code=STAGE_INVALID_ID/format`、`B\|UNTRUSTED\|code=SAF_PROVIDER_UNTRUSTED/authority`；取消 `B\|CANCEL\|code=STAGE_CANCELLED/user\|stageDirGone=true`（任何清理之前目录已经不在）；重复释放 `B\|IDEMPOTENT\|first=removed=true\|second=removed=true`；`B\|ACTIVE\|staleRemoved=0\|staleShape=int\|staged=ok\|payloadKept=true\|markerKept=true`；`B\|GRANT\|left=0\|journal=absent`。C 段——`C\|GRANT\|before=rw\|after=rw`，`C\|CLEANED\|removed=true\|stageDirGone=true`。**D 段（新）**——基线 `D0\|BASELINE\|status=ok\|copyMs=4780\|afterCopyMs=527`（256 MiB 的复制用了 4780 毫秒，到应答又过了 527 毫秒）；宿主在看到复制开始的标记后 1232 毫秒往 `nested/` 里加了一个文件，`D1\|RESULT\|code=STAGE_IO_FAILED/source-changed`、`D1\|VERDICT\|triggered=true\|stageDirGone=true`；宿主在 1188 毫秒处把 `photo.bin` 截到一半，`D2\|RESULT\|code=STAGE_IO_FAILED/size-mismatch`、`D2\|VERDICT\|triggered=true\|stageDirGone=true`；`D\|GRANT\|left=0\|journal=absent`，设备上的树已恢复成宿主的树。(2) **端到端，32 MiB**（03:27 到 03:39；就绪门 6 拍，最后一拍 0.94）：`ZSAF\|E2E\|RESULT\|发送完成：已发送 8 个文件（32.0 MB） · 互联网直连 / 用时 5 分 05 秒 · 平均 107 KB/s`，`STAGING_ROOT\|exists=true\|children=0\|stageDirs=0\|other=0\|scratch=false\|journal=absent`，`RECV_MATCH (8 manifest lines)`，`ROOT-VERDICT: clear`——这台模拟器上 32 MiB 传完要 5 分多钟，原先 4 分钟的预算不够。(3) **端到端，4 MiB**（04:55 到 05:03；就绪门 11 拍，最后一拍 0.90）：`发送完成：已发送 8 个文件（4.0 MB） · 互联网直连 / 用时 39 秒 · 平均 104 KB/s`，暂存根清空，`RECV_MATCH (8 manifest lines)`；测试在虚拟机服务起来 17 秒后开始。**此前没有通过的各轮（如实，逐轮；时间是发起时刻）**：探针 03:00（就绪门 5 拍）测试在虚拟机服务起来 24.4 秒后开始，产品行为与 (1) 相同（`source-changed` 与 `size-mismatch` 都触发），只是测试里两处比对写错——拿不带原因记号的错误码去比。端到端 03:12（就绪门 10 拍）与探针 04:11（脚本自己冷启动的那一轮，就绪门 30 拍）在宿主停掉驱动之前测试没有开始——应用进程起来了、虚拟机服务也起来了。端到端 03:39（就绪门 9 拍）测试在虚拟机服务起来 410.7 秒后才开始，那时宿主早已停止等待。探针 03:55 那一轮应用根本没有启动——ADB 掉线（`adb.exe: device offline`），与上一轮被停掉无关（执行方原先写成「系统选择器留在了前台」，是错的）。执行方在 03:39 那一轮通过虚拟机服务看过一次：isolate 处于 Resume 状态，Flutter 的扩展一个都没有注册——这只能说明观测那一刻扩展尚未注册，不能证明 `main()` 从未执行；测试恰在执行方查询该服务之后开始，是否有因果没有证实。**原因没有查明**：它发生在应用代码与测试代码运行之前。通过的三轮从虚拟机服务到测试开始分别是 11.8、16.7、19.0 秒，就绪门分别是 6 拍（106 秒）、4 拍（71 秒）、11 拍（211 秒）——就绪门的长短并不能区分通过与卡住（执行方原先写「通过的三轮都是三拍放行、卡住的之前门都耗时很长」，与原始记录不符）。codex 第二轮结束审核的判断：这一现象不构成产品批准阻断。**仍未验证**（同 Z4 行所列）：重复 document ID、虚拟文档、查询或读取异常、条目上限、溢出、空间不足、名称规则在设备上的拒绝、持久化条数到上限与读授权表失败、Activity 在暂存中途销毁、机器重启后的残留与 24 小时规则、归属日志在进程被杀之后的清理、取消打断阻塞中的 provider 调用。模拟器，不是物理设备 |
| 批次 Z 结束审核第二轮（codex 复审） | 本地 codex 会话（2026-11-25 出具，全程只读，659,658 tokens，复审区间 af71048..29f735b，结束时 HEAD 为 29f735b） | **总裁决：需改**。没有 P0：差异限于 Android 暂存器、Dart 网关与页面、测试和文档；没有 relay、TURN、文件上传或新依赖，FFI、`ffi-v2.md`、线上协议与清单权限没有变动。**第一轮四项 P1 关闭两项**：已知长度短读与复制后成员变化——逐文件长度核对、写标记之前的成员复核，D1、D2 的原始设备记录显示两种干预都触发了预期的错误且暂存目录被删；首页取消配对后旧选择仍被交接——回调复核页面、代际与连接，失效时释放它手里的条目。**两项未关闭（仍是合入阻断）**：读授权误撤——发送侧的查询、取、撤销与接收侧的授权授予没有跨线程协调，发送侧看到没有读位后取了读授权，接收侧随后对同一目录取得读写授权，发送侧收尾仍无条件释放读位；过期清理依据的是较早的快照。详情页发送中删源——`errorCodeOf` 是动态读取任意对象的整数 `code`，不等价于识别 `AkFfiException`，而引擎报告的失败也可能来自操作轮询；「已取消原生发送任务」只说明断开调用成功，Rust 侧的阻塞读取在 `spawn_blocking` 里——两类「已结束」的判定都可能提前解除保护。**P2**：清理结果与 ID 保护部分关闭——`cleanupStagedSend` 排在工作队列之后才判忙，暂存期间的定点清理会删掉刚交付的目录，Dart 的尽力释放不确认删除；Dart 回包归属按冻结契约关闭；两个集成测试的首轮漏检关闭（新增 P3：探针收尾的计数把无回包或 `removed:false` 也算作已应答）；原生异常与阻塞证据未关闭；STATUS 矛盾关闭；新增——已确认停止的发送失败若发生在页面销毁、清空或重选之后，只解除保护、不释放副本。**P3**：255 字节规则已加；`mkdir` 失败没有按 errno 归类；「每 isolate 一次」与过期扫描对符号链接的处理已改；两轮计划的裁决都是「需修改」、修订晚于并行实现开始，是流程偏差；分支没有从 main 切出，已披露，先合 X 再合 Z 可行；执行方把几种不同的启动经历混成一类——`0326`、`0433` 在终止前没有测试首标记，`0354` 在虚拟机服务起来约 411 秒后已开始测试，`0405` 是 ADB 离线而未能启动，`0312` 约 24 秒后进入测试；通过三轮的就绪门分别是 6 拍、4 拍、11 拍，不是每轮三拍；一次观测到 Resume 状态与零扩展不足以证明 `main()` 从未执行；**启动现象不构成产品批准阻断**。审核方核对：三轮通过的数字与清单相符；桌面的安装包与本机构建的 SHA-256 相同、签名核验通过；HEAD 的 CI 成功。审核方的结论：授权并发与详情页错误分类仍是两项 P1，都可以通过受控授权、工作队列测试与本机可阻塞的读取测试关闭，不依赖物理手机。**执行方处置**：第三轮修复见下一行；上面点名的记录错误已改正（上一行的后半部分） |
| 第三轮修复（codex 结束审核第二轮的处置） | 提交 14d7af7（原生）、f6d456e（页面）；集成测试的修订随复验提交 | 三路并行子代理按契约的第四版修订执行，执行方集成。**原生**：所有持久授权操作都在工作线程上串行——发送侧的查、取、撤销，过期清理，以及经原生通道 `pickFolder` 取读写授权的那一次。撤销按**当时**的权限位判断：只有读位仍在、没有写位才撤销读位；撤销之后立刻复读，如果接收侧恰好在这一瞬间把同一目录升级成读写、读位被削掉了，就把读位取回来。过期清理对归属日志里的每个 URI 用同一条规则，并把只剩写位的授权补回读位。每个暂存目录紧跟着建目录就写入进程令牌 `.owner`；过期清理删掉 ID 非法的项、不在进行中的半成品、以及不属于本进程的已交付目录——不再有 24 小时的规则，没有任何传输能活过进程重启，本进程交付的目录它不碰。`cleanupStagedSend` 在调用那一刻就判忙，什么都不排队。取消只置标志，取消信号与关闭输入流在专门的单线程执行器上做，关闭失败按异常类名记一行日志。建目录改用 `Os.mkdir`，ENOSPC 与 EDQUOT 归入空间不足。**页面**：详情页不再对失败做任何分类——参与过已开始发送的暂存条目只由**成功的发送**释放；任何失败都把它留在已选列表里等待重试并保持保护，之后的清空、重选、离页只把它移出列表、不释放，副本在下次启动时清掉；从没参与过已开始发送的条目照旧由清空、替换与销毁释放。**集成测试**：探针的收尾只把「已应答且 `removed=true`」的清理算作完成；新增三组设备检查——运行中的暂存收到定点清理必须立刻答 `busy`、随后照常交付；接收侧在一次发送暂存期间对同一目录取读写授权，结束时读写两位都在、没有只读授权；暂存中途杀掉应用，重启之后过期清理删掉半成品并撤掉日志里的只读授权。**门槛**：`flutter analyze`：`No issues found!`；`flutter test`：`All tests passed!`（506 项，上一轮修复之后是 488 项）；Kotlin 重编 `BUILD SUCCESSFUL`。子代理在仓库外的自检（不入库）：原生的宿主 JVM 纯逻辑检查 294 项全过；页面的变异检查 17 个全部被发现。**一个需要审核方知道的事实**：生产里接收保存目录的选取走的是第三方文件选择插件，它的授权默认是临时的、不持久化，应用也没有传选项；应用自己的 Dart 代码不调用 `pickFolder`。所以今天只有发送侧会写持久授权，第二轮审核担心的「接收侧在发送期间升级同一目录的授权」在现行生产代码里不会发生。**（执行方更正）**执行方原先在这里写「上面的串行化与按当前权限位撤销让两条路径都安全」，这句话在 Android 7 到 10 上不成立——执行方的独立复审发现了这一点，见下面「执行方的独立复审」那一行。**取舍**：进程令牌按进程生成而不是按暂存器实例（Activity 重建之后，同一进程里可能还有发送在读）；读位被加到一个原本只有写位的授权上、随后进程被杀——清理时这个读位会留下（应用本来就有那里的写权限，无害） |
| 跨进程的杀停检查（模拟器，profile 构建）——**通过** | 代码为提交 14d7af7；探针的 `leave` 与 `check` 两种模式；2026-11-25 06:52 到 06:58；原始文件留在本机 `target/device-evidence/kill-20261125-0657/`（不入库） | 新装应用之后连续两次启动，中间不清应用数据。**第一次（`leave`）**：宿主把系统选择器驱动到 `akstage/nested/deep`（依次点中 `akstage`、`nested`、`deep`、`Use this folder`、`Allow`，前台是 DocumentsUI 的选择器）；`ZSAF\|K\|START\|deep=absent\|readOnly=0\|journal=absent`（这个目录事先没有持久授权）；原始通道发起发送侧暂存，第一个复制事件处打出 `ZSAF\|K\|COPY\|entries=2\|estimate=268435471`；宿主看到该行 1075 毫秒后 `am force-stop`——此刻 `ZSAF\|K\|HOLD\|marker=false\|owner=true\|payload=true\|holdBytes=-1\|journalLines=1`（没有完成标记、有进程令牌文件、归属日志 1 行），复制停在 31457280 / 268435456 字节；这一轮的驱动以错误结束，是预期。**第二次（`check`）**：`K-KEEP` 应用仍在、数据保留；`ZSAF\|K\|BEFORE\|exists=true\|children=2\|stageDirs=1\|other=0\|scratch=false\|journal=98 bytes\|journalLines=1\|marker=false\|owner=true\|ownerBytes=33\|payload=true\|holdBytes=31457280\|grants=1\|readOnly=1`——被杀的进程留下恰好一个半成品暂存目录（带它的令牌文件）、归属日志 1 行、恰好 1 个只读授权；`ZSAF\|K\|SWEEP\|removed=1\|shape=int`；`ZSAF\|K\|AFTER\|exists=true\|children=0\|stageDirs=0\|other=0\|scratch=false\|journal=absent\|journalLines=0\|readOnly=0`；`ZSAF\|K\|DONE\|failed=0`；`All tests passed!`（00:07），`KILL-VERDICT: passed`，`TREE_RESTORED`。这一轮验证的是：进程在取授权与释放之间被杀之后，下次启动的过期清理会删掉它的半成品，并撤掉它记在日志里的只读授权。脚本的小问题：收尾之后遗留了一个读日志的 adb 进程，执行方手动结束，脚本已要求修正 |
| 第三轮修复后的暂存探针（模拟器，profile 构建）——A 到 D 段通过，E 段没有走完 | 代码为提交 14d7af7、f6d456e，集成测试为 988262f（运行时尚未提交，内容相同）；2026-11-25 09:11 到 09:27；原始文件留在本机 `target/device-evidence/probe-20261125-0927/`（不入库） | 就绪门 3 拍，最后一拍负载 1.25；应用是新装的。**A 段**：`ZSAF\|A\|STAGED\|name=akstage\|files=4\|bytes=33554461\|staged=true\|pathIsFs=true\|threw=no`，八个条目的类型、相对路径、大小与 SHA-256 与宿主清单一致（`ZSAF\|A\|CHECK\|ok`）；交付时 `ZSAF\|A\|STAGEDIR\|children=3\|payload=true\|marker=true\|owner=true`（`payload`、完成标记、进程令牌三样）；释放后 `ZSAF\|A\|RELEASED\|stageDirGone=true\|exists=true\|children=0\|stageDirs=0\|other=0\|scratch=false\|journal=absent`；`ZSAF\|A\|ROOT\|derivedOk=true\|resolved=true`；`ZSAF\|A\|DONE\|failed=0`。**B 段**：`ZSAF\|B\|BADID\|code=STAGE_INVALID_ID/format`、`ZSAF\|B\|UNTRUSTED\|code=SAF_PROVIDER_UNTRUSTED/authority`；取消 `ZSAF\|B\|CANCEL\|code=STAGE_CANCELLED/user\|stageDirGone=true\|events=2\|lastPhase=copy\|copyEvents=1\|cancelSentAtEvent=2\|bytesAtCancel=0`；重复释放 `ZSAF\|B\|IDEMPOTENT\|first=removed=true\|second=removed=true`；过期清理不碰进行中的任务 `ZSAF\|B\|ACTIVE\|staleRemoved=0\|staleShape=int\|staged=ok\|payloadKept=true\|markerKept=true\|ownerKept=true\|stageDirChildren=3`；`ZSAF\|B\|GRANT\|left=0\|journal=absent`；`ZSAF\|B\|DONE\|failed=0`。**C 段**：`ZSAF\|C\|GRANT\|before=rw\|after=rw`，`ZSAF\|C\|CLEANED\|removed=true\|stageDirGone=true`。**D 段**：基线 `ZSAF\|D0\|BASELINE\|status=ok\|copyMs=6398\|afterCopyMs=441\|estimate=268435485\|entries=8`；运行中的暂存收到定点清理 `ZSAF\|D0\|BUSY\|code=STAGE_IO_FAILED/busy\|answeredBeforeCopyEnd=true\|staged=ok\|rootExists=true\|marker=true\|owner=true\|payload=true`——复制结束之前就应答了 `busy`，暂存照常交付（标记、令牌、载荷都在），随后 `ZSAF\|D0\|CLEANED\|removed=true\|stageDirGone=true`；`ZSAF\|D1\|RESULT\|code=STAGE_IO_FAILED/source-changed`、`ZSAF\|D1\|VERDICT\|triggered=true\|stageDirGone=true`；`ZSAF\|D2\|RESULT\|code=STAGE_IO_FAILED/size-mismatch`（`filesCopied=2\|bytesCopied=161677326`）、`ZSAF\|D2\|VERDICT\|triggered=true\|stageDirGone=true`；`ZSAF\|D\|GRANT\|left=0\|journal=absent`；`ZSAF\|D\|DONE\|failed=0`。**收尾**：`TEARDOWN-VERDICT: clean — 9 ids, every cleanup answered with removed=true`（九个 ID 的清理都应答了 `removed=true`，收尾现在只把这样的清理算作完成）；`TREE_MATCH (8 manifest lines)`，`TREE_RESTORED`。**E 段没有走完（如实）**：应用这一侧是对的——`ZSAF\|E\|PICKED\|selected=true\|name=deep\|uriIsTree=true\|threw=no`、`ZSAF\|E\|PRE\|deep=absent\|akstage=rw\|readOnly=0\|journal=absent`、`ZSAF\|E\|START`、`ZSAF\|E\|PICKER2\|tree`（接收侧的第二次系统选择器在发送侧暂存进行期间打开）、`ZSAF\|E\|COPY\|entries=2\|estimate=1073741839\|journal=98`（发送侧开始复制时归属日志 98 字节，即一行）。失败在宿主对第二个选择器的驱动：宿主把它当成第一次导航的失败，盲重放点进了 `akstage/emptydir`，兜底路径在子目录里找不到回溯的入口，第二次选择没有完成，E 段的最终断言（取授权之后读写两位都在、日志里没有这个目录的行）没有跑到；驱动以 `+1 -1: Some tests failed.` 结束，`DRIVE-EXIT=1`。宿主导航的修法在仓库外，修好之后重跑的结果另起一行。模拟器，不是物理设备 |
| 第三轮修复后的端到端走查（模拟器，profile 构建，4 MiB）——**通过** | 代码为提交 14d7af7、f6d456e；2026-11-25 09:44 到 09:49；原始文件留在本机 `target/device-evidence/e2e-20261125-0949/`（不入库） | 就绪门 3 拍，最后一拍负载 0.19。`ZSAF\|E2E\|CODE_ENTERED\|path=controller-callback` → `ZSAF\|E2E\|JOIN_SUBMITTED\|send-entry\|****Q2ZK` → `ZSAF\|E2E\|PICKER\|tree` → `ZSAF\|E2E\|STAGED\|picked=1\|name=akstage\|exists=true\|children=1\|stageDirs=1\|other=0\|scratch=false\|journal=absent\|payloadOk=true` → `ZSAF\|E2E\|RESULT\|发送完成：已发送 8 个文件（4.0 MB） · 互联网直连 / 用时 32 秒 · 平均 124 KB/s` → `ZSAF\|E2E\|STAGING_ROOT\|exists=true\|children=0\|stageDirs=0\|other=0\|scratch=false\|journal=absent\|payloadOk=false` → `ZSAF\|E2E\|DONE`；`All tests passed!`，`DRIVE-EXIT=0`，`ROOT-VERDICT: clear`。宿主：`[recv] final ack verified=true`、`[task] received 8 file(s), 4.00 MiB`；`RECV_MATCH (8 manifest lines)`。模拟器，不是物理设备；速度是这台模拟器的数字 |
| 执行方的独立复审（codex 第三轮复审之前的自查；另一个只读审核者，不是 codex） | 2026-11-25 出具；对象是提交 14d7af7、f6d456e 与当时的集成测试（现为 988262f），对照契约第四版修订；全程只读（读代码与 AOSP 源码），没有在设备上运行；执行方逐条核对了它的平台事实 | **没有 P0；一项 P1、三项 P2、六项 P3。** N11 到 N14 与 U4 按修订落实；N9、N10 只在 Android 11 及以上成立。**P1——Android 7.0 到 10 上接收侧的授权会丢。** 平台事实（执行方已对着 AOSP 源码核实）：`UriPermission.releasePersistableModes` 在 android-7.0.0_r1、8.1.0_r1、9.0.0_r1、10.0.0_r1 里有一行 `persistableModeFlags &= ~modeFlags`——撤销一个权限位，就把它从「之后可以持久化的位」里一并去掉，只有新的一次选择器结果才会再给出来；android-11.0.0_r1 到 16 都没有这一行。`takePersistableUriPermission` 要求申请的位都在这个集合里（Android 10 的 `UriGrantsManagerService`）。后果：N9 把接收侧的取授权排在整次暂存之后，所以次序永远是「发送侧结束时看到只有读位 → 撤销读位 → 接收侧取读写两位」；在 Android 7 到 10 上，最后那一步抛 `SecurityException`，`takeReceiveGrant` 把它吞掉，`pickFolder` 照旧应答成功，接收目录没有留下任何持久授权（第三轮修复之前，同样的次序留下的是只有写位的授权）。模拟器是 Android 15，所以 T7 在它上面会通过；这项只在 API 24 到 29 的系统镜像上才会失败。**现行生产代码走不到**（应用的 Dart 代码不调用 `pickFolder`，暂存对话框是模态的）——是契约缺口，不是用户可见的回归；复审者认为如实披露并接受也可以按 P2 记。**P2 三项。** (1)「副本在下次应用启动时清掉」在系统复用进程时不成立——进程令牌是进程级的，Android 在最后一个界面退出后会留着空进程、下次启动直接复用；`MainActivity` 不用缓存的引擎，每次启动都会跑一遍过期清理，却把这些副本全部跳过；上一轮去掉的 24 小时规则原来会回收它们。今天能走到这一步而不经 Dart 释放的副本：发送失败之后的清空、重选或离页（U4 规定不释放）；页面还持有暂存条目时引擎被销毁（例如首页上已有选择、用户按返回键离开）。(2) T5 到 T8 的设备检查当时只在未提交的集成测试里；T7 的次序是「先撤销、后取」，在 Android 15 上把「写位在就把读位取回来」那段整个删掉它照样通过，对 N10 的回归不敏感；四张判定表（`afterReleaseStep`、`afterRestoreStep`、`sweepGrantStep`、`isStaleByRules`）没有入库的单元测试（本批不加测试依赖，§7）。(3) `MainActivity.kt` 里原有的几行日志带着 URI 或路径（第 407 行在本轮改写的 `onFolderPicked` 里），与「日志不带 URI、路径或名称」的规定相悖；另有第 357、397、438、537、543 行。**P3 六项。** 任务结束路径对只剩写位的授权直接划掉日志行、不补回读位（过期清理会补；在全部串行的前提下走不到）；Android 7 到 10 上「把读位取回来」不可能成功，这样的日志行每次启动都会重试并记一行日志，同一个 URI 的第二次发送侧取授权也需要重新选一次（生产每次都重选）；`lstatType` 把管道、套接字、设备文件都判成普通文件，`isOwnedByThisProcess` 会去打开 `.owner`，管道会让工作线程卡住（应用私有存储里走不到）；`pickFolder` 的应答现在要等工作线程上的事情做完，一次完整暂存也在内，`stager == null` 的分支不取授权就应答成功；四处过时的注释（`MainActivity.kt` 第 12、81 到 83 行，`file_pick_gateway.dart` 第 567 到 568 行，`app.dart` 第 77 到 81 行）；按契约保留的残余——失败之后的重试会发起第二次引擎发送，它成功时释放副本，而先前那次失败的 Rust 读取可能还活着。**复审同时核实为正确的**：Android 11 及以上授权竞争没有剩余窗口；判忙与稍后登记的任务；过期清理与发送；取消（同一个流只关一次，工作线程从不等取消线程）；应答只发一次、没有主线程异常路径、没有泄漏的流；路径安全（每个新建的条目都过包含检查，清理用 lstat 判型）；新增日志只带固定文本或异常类名；详情页的五个释放点都经同一个入口，失败发送的条目不会被清空、替换或销毁交还；把已确认与未确认的分类加回去、在 catch 里释放、去掉过滤、释放整份列表而不是快照、在 `_pickSource` 里解除保护——每一种改坏都至少让一个测试失败。**执行方处置**：以上各项连同执行方拟的修法交 codex 第三轮结束审核裁决（见下一行）；上面第三轮修复那一行里「让两条路径都安全」的说法已更正 |
