# G5b — E1 旧 Dart 服务层删除切片（2026-11-13）

分支：`dev/review-fixes-20261111`（AK-sending 工作树）。依据：验收决定 E1
（docs/REVIEW_2026-11-12-acceptance.zh-CN.md §6 第 131 行："现在就删除旧 Dart
服务层（上轮已核实从 main.dart 不可达）"）；删除方式遵循
docs/REVIEW_2026-11-11.zh-CN.md:244（导入图核实不可达后，连同只测旧层的测试一起删除）。

## 1. 从 main.dart 的可达性判定（导入图闭包）

main.dart 的本地 import：`services/ak_engine_facade.dart`、`services/dll_path.dart`、
`services/ffi_simple_transfer_service.dart`、`ui/app.dart`、
`ui/service/simple_transfer_service.dart`。逐文件展开传递闭包后，lib/ 下全部 23 个
Dart 文件中不可达的恰好 6 个：

| 文件 | 判定 |
|---|---|
| `lib/services/ffi_transfer_service.dart` | 不可达（仅旧层/旧测试引用）→ **删除** |
| `lib/services/invite_service.dart` | 不可达（wave-22 邀请码多码调度 UI 契约+实现）→ **删除** |
| `lib/services/real_transfer_service.dart` | 不可达（旧 EngineBackend 轮询泵）→ **删除** |
| `lib/services/transfer_service.dart` | 不可达（旧核心层契约/DeviceInfo）→ **删除** |
| `lib/ui/service/transfer_service.dart` | 不可达（旧 UI 契约，引用 invite_service）→ **删除** |
| `lib/services/simple_transfer_fake.dart` | 不可达但**保留**：main.dart 头注明确其为测试专用注入（LocalSimpleTransferService），被 4 个现行测试文件（w28_test_helpers/settings_test/direct_unavailable_guidance_test/diagnostics_export_test）使用 |

## 2. 候选存疑项的核实结果

- **`ak_engine_facade.dart`：活代码，未删。** main.dart 直接 import 且实际使用
  （`AkEngineDartFacade`/`AkStagedEngineOwner`/`AkStageLog`——bootstrapAppService
  的引擎拥有者，C07/C08/E4 均挂在它上面）。预研提示正确：这是新路径的生产组件，
  不是旧层。旧层删完后它的引用方只剩 main.dart（更干净）。
- **`connection_diagnostics.dart`（保护清单）依赖核实**：其引用的 `TaskEvent` 等
  类型全部来自 `package:ak_ffi_dart`，与旧层零依赖——保护文件不受本切片影响。
- 保护清单其余文件（simple_transfer_service / ffi_simple_transfer_service /
  dll_path）与 packages/、Rust 侧均未触碰。

## 3. 引用扫描（零活引用确认）

对 5 个删除候选在 lib/、test/、pubspec、analysis_options、scripts/、config/、
crates/、.github/ 全量扫描 import/export/符号引用：

- lib 内引用方只有彼此（5 个文件构成一个自闭连通分量）；
- 测试引用仅 3 个文件：`test/services_test.dart`（35 项）、
  `test/invite_service_test.dart`（10 项）、`test/fake_transfer_service.dart`
  （仅被 services_test 使用的旧契约 fake 助手）；
- 构建/CI/脚本零引用。

无任何生产活引用 → 满足删除条件，未触发"有活引用即停"。

## 4. 变更清单

删除（lib，5 个，即预研"约 5 个应用层文件"）：
- `flutter_app/lib/services/ffi_transfer_service.dart`
- `flutter_app/lib/services/invite_service.dart`
- `flutter_app/lib/services/real_transfer_service.dart`
- `flutter_app/lib/services/transfer_service.dart`
- `flutter_app/lib/ui/service/transfer_service.dart`

删除（只测旧层的测试，2 个）：
- `flutter_app/test/invite_service_test.dart`（10 项全部旧层）
- `flutter_app/test/fake_transfer_service.dart`（旧 TransferService fake 助手）

修剪（1 个）：
- `flutter_app/test/services_test.dart`：35 项 → 保留 6 项纯 dll_path 探测测试
  （三级探测 4 项 + Android tier-0 2 项）。原因：dll_path.dart 是生产仍用的保护
  文件，其纯函数探测的单测覆盖只在此文件；diag_stage_test/app_shell_test 只覆盖
  absoluteAkDllLoadPath/门面/C08 注入缝，不覆盖探测层级逻辑，整文件删除会丢覆盖。

文档同步（1 个）：
- `flutter_app/README.md`：架构图与关键文件表改为现行栈（facade → 任务面绑定 →
  FfiSimpleTransferService → simple_transfer_service），加 G5/E1 删除说明；
  "FfiTransferService 打开引擎"等旧主语与旧 API 文案（startReceive/startSend、
  测试命令注释）同步修正。

## 5. 验证

| 项 | 删除前 | 删除后 |
|---|---|---|
| `flutter analyze` | No issues found | **No issues found** |
| `flutter test` | 209/209（TEST_PLAN 第三十三轮记录） | **170/170 all passed** |

测试数差 39 = invite_service_test 10 + services_test 移除的 29（35−保留 6），
逐项对账一致，无丢失的非旧层测试。

## 6. 备注

- 变更未提交（deletions 已 git rm 暂存，编辑在工作区），待所有者复核后提交。
- 同工作树存在他人未完成改动（scripts/check_repository.py、
  scripts/tests/test_repository_checks.py、.tmp_analyze_sect7.py，CR-P3-10 相关），
  本切片未触碰。
- flutter_app/README.md 末尾"当前边界（wave 11）"一节仍为 wave-11 时代文案
  （deviceInfo/配对簿/listJobs 等早已过时），属整体文档陈旧问题，超出本切片
  范围，建议留给文档轮次。
