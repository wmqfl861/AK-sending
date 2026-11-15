# 批次 K 修复轮三独立复核（2026-11-15）

复核范围：`dev/batch-k-20261114` 的 `3db5259..31cf22c`，代码与文档修复为 `e8b621a`，哈希回填为 `31cf22c`。被审 tip 固定为 `31cf22c0d0583b7794e896aca6564ffd4d5ab0a7`。结论为 **REJECTED**；该轮未合并、未推送 main、未删除批次分支。报告及状态同步出具时仅保留本地，随后由执行方在 `46473fc` 提交，未改写该轮被审代码。本页保留历史结论，修复轮四的新判断见 [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md)。

逐项判断唯一记录在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)：K3-N1/K2-N1/K2-R2/K2-R3 对应 K-deploy，K2-R4 对应 CR-P3-20，K2-R5 对应 K-handoff。命令、计数、CI 运行编号与证据边界集中在 [K 交接 §13](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#13-修复轮三独立复核证据2026-11-15)。下列源码行号均指固定 tip。

通过当前配置的 git 凭据读取 GitHub run/jobs/branches API，确认 tip CI 已成功，完整 head SHA 与分支均匹配。无需为了记录运行编号再生成被审文档提交；本轮直接在本地交接证据表补录。绿门禁与以下额外行为反例分别判断。

## 1. 阻断发现

### K3-N1 / P1：缺失 unit 的标准 systemd 诊断未被识别，首次安装仍失败

`scripts/deploy_linux.sh:1166` 仅接受 `could not be found`、`not loaded`、`No such file` 为明确不存在。`systemctl cat` 对缺失 unit 的标准诊断是：

```text
No files found for ak-registry-server.service.
```

上游 systemd v255 的 [systemctl-util.c:534](https://github.com/systemd/systemd/blob/v255/src/systemctl/systemctl-util.c#L534) 将 `LoadState=not-found` 转入 finish，[同文件第 611 行](https://github.com/systemd/systemd/blob/v255/src/systemctl/systemctl-util.c#L611) 输出上述文本；[systemctl-edit.c:53](https://github.com/systemd/systemd/blob/v255/src/systemctl/systemctl-edit.c#L53) 的 `cat` 调用链在返回无文件时设置 `-ENOENT`。这与脚本注明的 Ubuntu 24.04 目标相关。两份上游源码已在仓外保存；本机没有原生 systemd，未将源码核对描述成实机部署。

真实生成的 phase456 在当前 stub 文案 `Unit ... could not be found.` 下能完成安装。只将 `scripts/tests/test_deploy_linux_install.py:189` 的缺失文案换成上述标准输出，保持同一不存在 binary/unit 的 fixture，候选即输出 `systemctl cat unusable`，在切换前退出，binary/unit 没有安装。由 git 提取的 `f150503` 自身 phase456 在同一标准文案 fixture 中能完成安装。

执行方的缺失 unit 回归实际执行了生成脚本，但 stub 的诊断文案与目标工具不符，不能据其成功关闭首次安装退化。修复应依据 systemd 明确的加载/存在状态区分 absent 与查询失败；若仍解析文本，须覆盖目标版本和 locale，并避免把连接/权限错误当 absent。真实 stop 失败仍应严格拒绝。

### K2-N1 / P1：恢复时查询失败仍被当成 inactive，留下新版进程却报告恢复成功

部分启动后原 active 服务改用 restart、原 disabled/inactive 状态保留、旧 binary rename 失败后的备份兜底，都已通过现有状态性回归。但 `scripts/deploy_linux.sh:1297` 对恢复时 `systemctl is-active` 仍只按 shell 真假分支；查询失败会落到“无需停止”的路径，既不标记不完整，也不验证最终 inactive 状态。

可复现前置为旧 registry/STUN 均 disabled/inactive；状态捕获与停服前查询成功。令 `enable --now` 成功启动新版 registry，随后 STUN 启动失败；只令 registry 恢复阶段的 `is-active` 查询无 stdout、非零退出，模拟无法查询，其他恢复命令正常。执行真实 phase456 后，磁盘及 enable 状态恢复，但最终：

```text
RECOVERY OK: previous binary/units restored, enable/active state re-asserted and running images verified
registry enabled = 0
registry active = active
registry running.bin = git deadbee（新版合成镜像）
```

脚本整体失败退出并不能抵消错误的恢复成功声明：恢复目标要求 registry 回到 inactive，而新进程仍运行。`:1262` 的 absent 分支也存在按查询真假跳过 stop 的同类结构；此分支只作静态指出，不扩成已独立执行的另一反例。

捕获端另有边界：`:1199` 仅拒绝空字符串，未校验输出是否属于可确认、可恢复状态。用已有 stub 输入非空 `unknown`，真实 phase456 仍记录 `enabled:unknown` 并完成切换；这是合成未知输入实验，不声称每个 systemd 版本都会在该前置下返回此值。

修复要求：恢复查询也必须保留原始状态与错误语义；无法判断或无法确认最终状态时报告 `RECOVERY INCOMPLETE`。对原 inactive/absent 的 unit，应可靠停止本次启动的进程并确认结果；捕获时对未支持或不可判定状态在停服前拒绝。补充“捕获成功，部分启动之后恢复查询才失败”的状态性回归。

### K2-R3 / P1：标准权限错误仍被当成身份缺失，已知旧 unit 也可漏出迁移集合

存在性 helper 的 `scripts/deploy_linux.sh:963` 是：

```bash
*"No such file or directory"*|*"cannot access"*) echo no; return 0 ;;
```

GNU ls 的正常权限诊断 `ls: cannot access '…': Permission denied` 含有 `cannot access`，因此被误判为 absent。分别让已存在的合成私有身份目录，或其 `registry.key`/`registry.db` 的 `sudo ls -d` 返回该诊断；端口空闲，sudo true、home 枚举和其他探测全部成功。真实 phase1 不再报告已有身份，提取的真实 `real_run` 决策块继续完成，输出 `existing registry data dir: none-found`。仓内选择性失败 stub 使用 `sudo: ls: blocked by stub (forced)`，没有覆盖真实 ls 错误形状。

旧 registry 归属仍有另一个相同要求下的缺口。`:923` 仅在 `sudo test -f /etc/systemd/system/$reg_unit` 成功时输出 `old-registry-unit`；这个条件无法区分权限错误，也遗漏 `/usr/lib/systemd/system` 等合法 unit 路径。分别注入该 test 的权限失败，或将合成旧 unit 放在 vendor 目录，cgroup 和 cmdline 都正常读取；真实 phase1 明确显示：

```text
port 17200/tcp: in use (pid 4242, unit oldregistry.service)
```

但 `:1559` 只从缺失的 `old-registry-unit` 字段取迁移对象，`:1574` 后只拒绝 `not-a-systemd-unit`，实际决策仍通过并输出 `registry-port occupant unit: none-detected`、`existing registry ExecStart: none`。因此已确认的旧 registry 不进入后续停止/迁移集合。这里机械执行到真实决策块为止，没有把推断的后续部署影响写成已部署结果。

本轮 cgroup/home 枚举错误、pid-unknown/非 systemd registry 拒绝和无换行 cmdline 的修复确实有效，以上是其剩余边界。修复应可靠区分 ENOENT/ENOTDIR 与权限/I/O 等不可判定结果，优先使用结构化结果；若必须解析诊断，固定 locale 且仅接受明确不存在。已从 cgroup 确认的占用 unit 必须取得 systemd 实际加载配置并绑定迁移集合，无法取得时拒绝，不能因硬编码目录查询失败而忽略。

### K2-R4 / P2：单行链接顺序问题已修复，逐行扫描新增合法跨行链接漏检

原 K3 反例已翻转：规范链接在前、archive 链接在后时，新完整检查器逐处报出行号；颠倒顺序也拒绝。历史链接指向不存在文件时同样拒绝。但 `scripts/check_repository.py:313` 将 STATUS 先 `splitlines()`，下一行再逐行执行 Markdown regex，导致合法的跨行 label 被完全漏过。

从 git archive 分别导出旧、新完整树，创建真实 archive 同名文件，内容为既有矛盾规则会拒绝的“完成声明＋待补证据”。STATUS 的“当前状态”放 label 跨行的 archive 链接，“最新验证”与“交接入口”各保留规范链接。下面生成精确合成输入；不在本仓库新增实际 archive 文件：

```python
name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
archive = "[" + "K\n交接" + "](archive/" + name + ")"
canonical = "[" + "K" + "](" + name + ")"
status = ("# 项目状态\n\n## 当前状态\n\n" + archive
          + "\n\n## 最新验证\n\n" + canonical
          + "\n\n## 交接入口\n\n" + canonical + "\n")
```

`3db5259` 完整检查器识别首个 archive 链接并拒绝；`31cf22c` 完整检查器成功退出，`passed=true`、`errors=[]`，只检查规范文档。通用 broken-link walk 能识别全文链接，但 archive 文件确实存在，所以不会补上专用检查的遗漏。这是本轮新增回归，不能以原顺序反例已修复覆盖。

另一个带合法 Markdown title 的 archive 链接在两版均被漏过，属于既有解析限制。修复可以全文匹配后由 match 起始位置计算行号，并对支持的 Markdown 链接形式作一致解析；不能靠未强制的“仓库 label 不跨行”注释跳过输入。补充完整检查器的跨行及 title 场景。

## 2. 保留的有效修复与声明边界

K2-R2 的严格 stop 与 phase7 的 MainPID→`/proc/<PID>/exe` 核验维持上一轮结论；旧路径与 deleted executable 回归继续通过。K2-N1 的既有部分启动、合法 disabled/inactive、rename 失败兜底回归均通过，旧 `f7dece2` 的对应行为反例也确实从 git 提取执行。上节的新恢复查询反例不否认这些进展。

K2-R5 的 skip-doctor 摘要、实际旧 SSH helper 和旧部署阶段对照仍有效。完整 Python 套件实际执行了指定基线，没有以缺失基线跳过这些用例。缺失子目录此前已被旧完整检查器的通用 broken-link 规则拒绝，这一声明现在已准确限定。

“只有 git 无法提供 baseline 才 skip，其他情况 AssertionError”仍不能泛指部署套件：`scripts/tests/test_deploy_linux_install.py:660` 的 `section` 支持 `skip_if_missing=True`，`:1434` 起的 baseline 阶段提取实际传 True，decision block 也有同类路径。对可读但缺 marker 的合成 baseline，部署提取实测抛 `SkipTest`，SSH helper 提取实测抛 `AssertionError`。指定旧基线 marker 均存在，这不推翻本轮实际完成的行为对照；本轮在交接中限定声明。

K3 报告、§11 与入口同步确实随 `e8b621a` 入库，`31cf22c` 也确实仅回填哈希。但候选交接开头、§11 以及审核状态表尾注仍写 K3 “仅本地/未提交”，K-handoff/K-CI/main 行仍停在上一轮。此次本地同步明确“出具时本地，随后随 e8b621a 入库”，更新本轮状态和证据入口。CI run 未再单独提交不构成发现。

## 3. 可重放材料与后续处理

本轮仓外复核脚本均已保存，从仓库根目录执行：

```powershell
python -u "$env:TEMP\ak-k4-recovery-repro.py" .
python -u "$env:TEMP\ak-k4-privilege-repro.py"
python -u "$env:TEMP\ak-k4-checker-replay.py"
```

恢复与权限脚本使用当前 ProbeHarness 提取真实 generator/真实决策块，只修改合成夹具和工具响应；恢复脚本还从 git 提取旧阶段并保存上游 systemd 源码。checker 脚本从 git 导出两版完整树，逐场景运行完整检查器。脚本执行成功表示复现流程完成，反例的实际阶段退出和最终状态在输出/JSON 中，不能将 runner 的 exit 0 当成部署验收通过。每次运行会输出新的 EVIDENCE 目录；本轮实际重放目录见交接证据表。

后续在现有批次分支修复并加入上述行为回归，再交独立复核决定合并。本轮只同步文档，保全候选代码和本地/远端 main。未连接生产或远端 SSH，未运行真实 Linux systemd 部署、物理双机或抓包验收；离线 stub 不构成真实身份、真实路径或权限证明。ADR-001 与禁止 relay/代理回退的产品决策不变。
