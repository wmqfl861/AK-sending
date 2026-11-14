# 批次 K 修复轮二独立复核（2026-11-15）

复核范围：`dev/batch-k-20261114` 的 `f150503..3db5259`，代码与文档修复为 `f7dece2`，哈希回填为 `3db5259`。被审 tip 为 `3db5259a3a56704e56709c0d615287081d17c055`。结论为 **REJECTED**；该轮未合并、未推送 main、未删除批次分支。报告及同步出具时只保留本地，随后由执行方在 `e8b621a` 提交。本页保留历史结论，修复轮三判断见 [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md)。

逐项判断唯一记录在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)：K2-N1/K2-R2/K2-R3 与新增 K3-N1 对应 K-deploy，K2-R4 对应 CR-P3-20，K2-R5 对应 K-handoff。命令、计数、运行编号与证据边界集中在 [K 交接 §11](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#11-修复轮二独立复核证据2026-11-15)。以下源码行号均指被审 tip，不指本地文档同步后的行号。

本次通过配置好的 git 凭据从 GitHub API 核对了 tip CI，完整 head SHA 一致且已成功。没有因交接尚未记录该运行而要求再造文档提交；运行记录由本次本地同步补入交接。CI 成功与下述脚本反例分别判断。

## 1. 阻断发现

### K2-N1 / P1：恢复覆盖了调用范围，仍未恢复实际运行状态

完整切换确实已进入 `do_switchover`，unit 写入、reload 和 enable/start 的返回错误也会调用恢复。但 `scripts/deploy_linux.sh:1094` 只恢复磁盘旧文件，`:1113` reload，`:1119`–`:1120` 对原 active 的服务只执行 `start`。恢复没有停止本次已启动的新进程；systemd 对 active 服务的 `start` 不会重启它。

可复现前置：两个旧服务 enabled/active；正常停服、换新文件并写 unit；让 `enable --now` 先启用两服务并成功启动新 registry，再因 STUN 启动失败返回非零。真实生成的 phase456 随即恢复旧磁盘和 unit，`start registry` 对仍 active 的新进程无效，STUN 则由旧磁盘启动。脚本失败退出却打印 `RECOVERY OK`，最终 registry 仍为新映像、STUN 为旧映像。磁盘恢复不等于进程恢复。

另一个初始状态为 disabled/inactive 的变体也失败：`:1139` 和 `:1141` 将 `systemctl is-enabled`/`is-active` 的合法非零状态输出清空，记录成 unknown。部分启动失败后恢复既不 disable 新启用的服务，也不停止原先 inactive 的服务，仍打印恢复成功。执行方测试中的 systemctl stub 不维护启用、运行或进程版本状态；其 enable/start 注入在产生任何副作用之前失败，因此上述状态错误没有被现有断言捕获。

二进制保存还有独立缺口。`:1171` 仍是：

```bash
[ -f $REMOTE_BIN ] && sudo -n mv $REMOTE_BIN $REMOTE_BASE/ak-cli.old-\$TS || true
```

这不是每一步都 `|| return 1`。令旧二进制 rename 失败、随后新文件 rename 成功，再令第一次 unit 写入失败：`recover_services` 只查 `.old-$TS`，没有使用停服前已经保存的 `$BACKUP/ak-cli`。实际恢复后磁盘及两个重新启动的服务都仍为新版本，但日志打印 `RECOVERY OK`。原备份存在，恢复却未利用。

修复要求：保存并区分 absent/disabled/inactive/active 与查询失败；失败恢复要处理切换已产生的部分副作用，停止本次新进程，恢复可靠备份和原启用状态，按原状态启动并验证运行映像。任何无法确认的恢复都应报告不完整。增加部分 enable/start 成功、原服务 disabled/inactive、旧 binary rename 失败的状态性回归，不能只断言调用日志里出现 start/enable。

### K3-N1 / P1（新增）：严格 stop 无条件作用于不存在的 unit，首次安装退化为必然失败

`scripts/deploy_linux.sh:1151`–`:1152` 在创建新 unit 之前无条件执行严格 stop。首次安装或只有其他名称的 legacy unit 时，`ak-registry-server.service`/`ak-stun-server.service` 尚不存在；systemd 的 unit-not-loaded 非零返回导致直接进入恢复退出，后面的 binary/unit 安装不会执行。

同一无 managed unit、无旧 binary 初始状态与同一 systemctl 模型下，分别从 git 提取 `f150503` 与被审候选各自的 phase456：旧候选完成安装，新候选在首个 stop 报 `Unit ak-registry-server.service not loaded.` 后中止。仓内成功安装测试把所有 stop 默认设为成功，覆盖不到此正常初始状态。

修复要求：先可靠识别 unit 是否存在及是否需要停止。已存在且需要停止的服务必须保留 K2-R2 的严格失败处理；已确认不存在的 unit 应允许继续安装。补充首次安装与只有 legacy unit 的迁移回归，不恢复无条件吞 stop 错误。

### K2-R3 / P1：sudo 成功路径改善，但未知身份和占用者仍可通过预检

新增 sudo 能在成功读取时发现他人进程和私有目录，`ss` 整体失败也会拒绝。其余关键错误没有获得同样处理：`scripts/deploy_linux.sh:887`/`:889`、`:902`/`:904` 的 `/proc` 读取及 `:916` 的 home 枚举通过管道或 `|| true` 吞错，`:918`–`:920` 的存在性探测将错误等同 absent。`sudo -n true` 成功不能证明其他命令获准或目标可读取。

独立复现既执行真实 phase1，也执行 `real_run` 中从 `ERROR` 检查到 `CHECK_ONLY` 的实际决策块：

- 保持 `sudo -n true` 成功，只拒绝 `sudo -n ls -1 /home`，私有合成 key/db 确实存在，端口当前空闲。输出没有 `registry-data:`，后续部署决策接受空 data。
- 目录枚举成功，只拒绝私有 key/db 的 `sudo -n test -e`，同样把已有身份当成未发现并继续。
- registry TCP 占用和 PID 已知，只拒绝读取该 PID 的 cgroup，STUN 端口空闲。输出 `unit not-a-systemd-unit`，后续仍接受空的 old_registry。外层只拒绝未知 STUN 占用，没有对 registry 做相同拒绝。

成功发现链还有解析缺口：Linux `/proc/<PID>/cmdline` 没有末尾换行，`:889` 的 NUL→空格输出会与 `:891` 的机器字段拼接：

```text
  cmdline: /opt/ak-sending/ak-cli registry-server   old-registry-unit: oldregistry.service
```

`:1381` 用 `^  old-registry-unit:` 解析，因此实际得到空的 `old_registry_unit`。无任何失败注入，执行方自己的正向 fixture 即可复现。测试只检查子串存在，没有把发现输出传入真正 parser；后续 phase456 便不会把真正的旧 registry unit 纳入停服、备份和停用集合。此缺口在前序代码已存在，本轮仍属于既有发现链未收口。

修复要求：区分不存在、存在与无法读取；任何影响占用者、身份或数据库判断的未知结果都要在切换前拒绝。机器字段必须有明确记录边界，回归应覆盖 phase1 输出到 real_run 决策的完整链。上述都是离线权限模型，不是实际 Linux 权限验收。

### K2-R4 / P2：同批次多个不同链接目标时，只检查第一个

单个链接的路径解析已改进，但 `scripts/check_repository.py:331` 收集候选后，`:339` 只按日期和批次取 `max`，同批次平手返回第一个候选；`:340` 只比较这一个目标的规范路径。STATUS 后面的同批次链接可以指向另一份未检查文档。

完整检查器复现：从 `git archive 3db5259` 导出树，保留 STATUS 第一个 K 链接指向规范文件，将“最新验证”和“交接入口”的 K 链接改为 `archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md`；创建该真实文件，内容为既有矛盾规则会拒绝的两行（一行完成声明、一行待补证据）。完整检查器仍成功退出，报告检查的是 `docs/HANDOFF-BATCH-K-2026-11-14.zh-CN.md`，错误列表为空。

将 archive 链接放在最前、规范链接放在最后，候选立即报规范路径不符及 archive 矛盾；直接对 archive 执行矛盾检查也会报错。检查结果取决于链接顺序，仍不满足实际入口锚定要求。

修复要求：所有指向当前最新批次的链接必须一致指向规范目标；同批次存在多个实际目标时显式拒绝，不能按出现顺序忽略其余目标。加入两个顺序的完整检查器回归，并让 run/矛盾门槛与实际入口一致。

## 2. K2-R2、K2-R5 的有效修复与证据修正

K2-R2 的原反例已经被处理：真正 stop 失败会在二进制切换前进入恢复；phase7 保留 ExecStart 检查，并通过 MainPID 和 `sudo -n readlink /proc/<PID>/exe` 识别旧路径与 `(deleted)` 映像。已执行的回归保留“旧进程运行、磁盘路径已换”前置，新代码拒绝该状态，前序候选会假成功。新增的首次安装回归和恢复状态缺口分别归 K3-N1、K2-N1，不否认这项进程核验的改善。

K2-R5 的摘要与旧码执行证据已经改善。跳过 doctor 时，实际摘要使用 observed path/no NAT observation；直接执行旧 helper 会表现为零超时失败，新 helper 能完成延迟和空输出命令。部署基线对照确实执行 `f150503`、`f9582e5` 各自生成的 phase1/phase456/phase7，指定基线不存在旧轮的准备阶段崩溃问题。SSH 旧码结果包含行为断言和结构断言，不能把所有失败都称为行为失败。

候选文档仍有遗留措辞：K 交接开头、§8/§9 与状态表尾注仍称 K2 同步“本地未提交”，而它已随 `f7dece2` 入库；状态表仍写哈希待回填，而 `3db5259` 已完成回填。STATUS 的最新证据入口仍停在上一轮。本次按收尾职责纠正历史归属和新一轮入口，不据此重写被审提交，也不以本地修正文档替代候选代码修复。

另外两项执行方声明需要准确限定：缺失子目录反例在旧专项函数中被 basename 替换而放行，但旧完整检查器已由通用 broken-link 门槛拒绝；不能写成旧完整检查器接受全部单目标反例。部署测试的 `ProbeHarness.section` 仍用 `str.index`，缺 marker 会抛 `ValueError`；只有 git revision 不可用才 skip。指定基线阶段均存在，所以这不推翻实际完成的旧码对照，也不新增产品阻断。

## 3. 可重放材料与后续处理

从本仓库根目录运行以下仓外复核脚本。恢复与首次安装脚本使用当前 `ProbeHarness` 提取真实 generator，首次安装对照另从 git 取前序候选；权限脚本执行真实生成阶段和实际后续决策；checker 材料保留误接受的完整导出树。全部 fixture 是合成数据，特权工具仅为 stub。

```powershell
python -u "$env:TEMP\ak-k3-recovery-repro.py"
python -u "$env:TEMP\ak-k3-fresh-baseline-repro.py"
python -u "$env:TEMP\batchK3-privilege-repro.py"
python "$env:TEMP\ak-k3-checker-evidence-27iv0s4y\reproduce_checker.py" . "$env:TEMP\ak-k3-checker-evidence-27iv0s4y"
python "$env:TEMP\ak-k3-checker-evidence-27iv0s4y\tree\scripts\check_repository.py"
```

首次安装脚本依赖同目录的恢复脚本；最后一个命令在有缺陷的候选中成功退出正是反例，不是验收通过。外部材料及本轮实际结果索引见交接证据表。后续执行方应将上述反例纳入仓内状态性回归，修复后在同一批次分支提交，再由独立复核决定合并。

本轮没有生产部署、远端 SSH、物理双机或抓包验收。ADR-001、无 relay 与文件数据仅端到端直连的约束未改变；没有把模型输入或本地 stub 结果描述为真实身份、真实路径或 systemd 实机证明。
