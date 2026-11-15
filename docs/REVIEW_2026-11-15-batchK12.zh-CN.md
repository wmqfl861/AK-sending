# 批次 K 修复轮十一独立复核（2026-11-15）

复核范围：`dev/batch-k-20261114` 的 `09fceb6..1fad0a2`；实现提交为 `2d77eed`，`1fad0a2` 只回填交接与状态表两行哈希。被审 tip 固定为 `1fad0a2d6814e6f19fa931960731cf2afe7dd68e`。结论为 **REJECTED**：原 K11-N1 晚提交反例已翻转，但 K11 报告已经记录的 K10-N1 运行目录识别问题仍可复现；另有本轮引入的权限命令失败漏处理。

逐项状态只在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。命令、计数、GitHub 运行编号和证据边界集中在 [K 交接 §29](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#29-修复轮十一独立复核证据2026-11-15)。本轮新增报告并同步本地文档，不修改被审代码、不提交、不合并或推送 main、不删除批次分支。源码行号均指固定 tip。

已使用当前 git 凭据直接查询 GitHub run/jobs API。首次查询时 tip CI 已 completed/success，无需继续等待；完整 head SHA、分支、各 job 与常规步骤均逐项核对。手动 release 深测的跳过不计为执行。绿色 CI 不覆盖下面的独立反例。

## 1. K10-N1 / P1：运行身份目录问题仍在，不能按“双双通过”关闭

请求背景、STATUS 和执行方交接 §28 使用了“K10-N1/N2 双双判通过”的摘要，但提交内 [K11 报告 §1](REVIEW_2026-11-15-batchK11.zh-CN.md#1-k10-n1--p1原输入已修复但仍错误确认运行身份目录) 与状态表明确保留 K10-N1 阻断。本轮 diff 没有修改该段来源识别逻辑。独立复现确认这不只是文档残留。

用 K11 报告 §5 的原始 fixture、逐参数 NUL argv、真实 phase1／决策／compose／安装链，当前和上轮候选得到相同结果：

| 输入 | `09fceb6` 与 `1fad0a2` 的实际行为 | 应有依据 |
|---|---|---|
| 真实 NUL 环境记录中的 `USERPROFILE=/home/otheruser\nsuffix`，unit 使用 EnvironmentFile | phase1、decision、install 均成功；选择 `/home/otheruser/.ak-sending`，目标为 prefix 目录的合成 key/db，新 unit 指向目标 | CLI 的 `var_os → PathBuf → join` 保留 LF，不能把含 LF 的路径截成另一个目录 |
| USERPROFILE 存在但为空；配置 WorkingDirectory 为 `/home/configured-new`，运行 cwd 模型为 `/home/runtime-old`；两目录有不同合成数据 | 各阶段均成功；选择 `/home/configured-new/.ak-sending`，目标为 configured-decoy，新 unit 指向目标 | 空变量得到相对 `.ak-sending`，须以当前运行实例实际 cwd 解析 |

`scripts/deploy_linux.sh:1231`–`:1232` 的 `grep -za` 后仍接 `head -n 1`，重新按 LF 截断真正变量值；`:1286`–`:1293` 仍根据 `systemctl cat` 的配置 WorkingDirectory 拼路径，没有读取 `/proc/<PID>/cwd`。`:2060` 的后续逐行消费也要求端到端处理路径字节，不能只替换一个过滤命令。

专项和集成者分别重放后，当前输出都包含 `cmp-verified post-stop`：时序修复已经生效，但它复制的是错误来源。cwd readlink 阳性控制可成功返回 `/home/runtime-old`，实际安装链未查询；这排除了“harness 无法提供实际 cwd”这一解释。从 `crates/ak-cli/src/main.rs:709`–`:716` 精确提取的默认链经 rustc 编译执行，确认值内／值尾 LF 保留、空值返回相对 `.ak-sending`。

原 K10 的 EXTRA 内 LF、空 USERPROFILE 加配置目录、缺配置目录拒绝回归保持通过；它们没有覆盖上面的实际变量 LF 和配置／运行 cwd 差异。这是延续 K11 已确认的未完成项，不归为本轮新引入。

**修复要求：** 取得与当前运行实例绑定的 cwd；无法确认时拒绝。路径字节须端到端保留，或明确拒绝当前表达方式不支持的输入；不得截断后确认另一个目录。保留原已通过回归，并对上述输入验证最终来源、unit 和落盘数据。

## 2. K12-N1 / P2：私钥权限设置失败被吞掉，仍切换并返回成功

这是 `2d77eed` 引入的回归。`scripts/deploy_linux.sh:1838` 将以下语句从顶层移入 `do_switchover`，却没有像相邻 chown、目录 chmod 一样显式返回失败：

```bash
sudo -n test -f $REMOTE_DATA/registry.key 2>/dev/null && sudo -n chmod 600 $REMOTE_DATA/registry.key
```

`:1907` 以 `if ! do_switchover` 调用函数，Bash 对整个函数体禁用 `errexit`。函数自身在 `:1772`–`:1775` 已说明这一规则。前版相同 chmod 位于顶层，作为 `&&` 的最后一项失败会触发 `set -e`；移入函数后，其非零结果被后续成功命令覆盖。

用提交内 upgrade 状态模型，只让 `chmod 600 .../data/registry.key` 返回 1，其他工具保持原行为。专项与集成者分别执行同一组新旧版本、正常／失败控制：

| 固定版本与输入 | 安装结果 | 服务与恢复结果 |
|---|---|---|
| `09fceb6`，正常权限命令 | `INSTALL_RC=0` | 两服务 active，运行新 image |
| `09fceb6`，仅私钥 chmod 失败 | `INSTALL_RC=1` | 未 stop；两服务保持 active、旧 image |
| `1fad0a2`，正常权限命令 | `INSTALL_RC=0` | 两服务 active，运行新 image |
| `1fad0a2`，仅私钥 chmod 失败 | **`INSTALL_RC=0`**，输出明确包含 chmod 失败 | stop 已发生，仍替换二进制／unit 并启用新 image；没有进入 recovery |

因此不能宣称“迁移和权限处理的全部失败均进入统一恢复”。本发现证明权限设置失败仍被当作成功部署，不声称已经发生密钥泄露：目录 `chmod 700` 有显式错误处理，隔离模型也没有验证真实 Linux 权限或用户可读性。

**修复要求：** 区分“合法全新安装尚无 key”与“已有 key 的 chmod 失败”。例如在存在分支内显式 `chmod 600 ... || return 1`，令失败进入统一恢复。回归须验证非零安装结果、恢复结果、旧二进制／unit／active 状态，而不是只搜索 chmod 文本。

## 3. K11-N1 的实际修复与 K10-N2 恢复证据

源码顺序已变为停止 managed／旧 registry／legacy units，然后执行身份和数据库最终迁移，随后替换二进制、写 unit、reload 和 enable。目标一致保留、不一致拒绝、缺失时复制加 cmp、全新脏目标拒绝与原位升级分支保留；这些冲突或复制错误现在通过 `return 1` 进入 `recover_services`。

独立重放 K11 报告中的真实 schema-v3 SQLite DDL，在 `stop oldregistry.service` 入口、旧服务仍 active 时提交 `state='revoked', revision=revision+1`。当前目标与来源均为 `revoked / 2`，无晚提交控制均为 `active / 1`；完全相同 fixture 的 `806ea6c` 目标仍为 `active / 1`，来源为 `revoked / 2`。stub 日志机械核对为 stop → 来源 cp → cmp；新 unit 的默认库确实指向目标。

测试精度补注：提交内 `test_k11n1_late_commit_included_in_post_stop_copy` 虽然调用 stop-entry hook，但其 `make_upgrade_model` 只把 managed units 设为 active；新增的 oldregistry unit 只有文件。单独留档运行显示捕获状态为 `oldregistry:exists:disabled:inactive`，hook 本身也未断言 active。独立原报告重放显式设置旧服务 enabled/active/PID，专项还在 hook 中断言 active，因此上述时序判断有补齐前置条件后的实际证据；这项 fixture 精度问题不另列阻断，提交内回归应补齐相同条件。

复制损坏、目标不同的拒绝回归实际验证安装失败、恢复完成、服务 active 且运行旧 image；全新脏目标验证空恢复结果；一致目标、干净复制和原位升级控制也实际运行。原 K11-N1 晚提交反例不再作为此次拒绝原因，K12-N1 的权限命令遗漏另列。

上述证据为真实 Bash 生成器、实际临时文件与 SQLite 事务，加 systemctl／proc／权限模型；未启动真实 registry 执行协议 revoke，未执行 Linux 原生 systemd 停服、SSH 或生产部署。它证明已复现的正常提交交错得到修复，不扩为任意数据库崩溃状态的恢复证明。

## 4. 可复现步骤

在固定候选仓库根目录使用 PowerShell。以下从已提交的 K11 报告机械提取完整 runner，只把当前版本从 `09fceb6` 换为 `1fad0a2`，保留 `806ea6c` 对照和全部输入。读取各 CASE 的实际 `CHOSEN_DATA_SOURCE`、`INSTALL_RC`、`DATA` 和 unit；runner 退出 0 只表示重放完成。

```powershell
@'
import re
from pathlib import Path
report = Path("docs/REVIEW_2026-11-15-batchK11.zh-CN.md").read_text(encoding="utf-8")
script = re.search(r"```powershell\n@'\n(.*?)\n'@ \| python -\n```", report, re.S)[1]
script = script.replace('"09fceb6"', '"1fad0a2"')
exec(compile(script, "K11-original-at-1fad0a2", "exec"))
'@ | python -
```

权限失败的最小完整安装链复现如下。只在临时 harness 中新增 chmod stub，生产生成器逐字取自真实 git 版本，所有文件均为合成数据。

```powershell
@'
import importlib.util, json, re, subprocess
from pathlib import Path
spec = importlib.util.spec_from_file_location("deploy_tests", Path("scripts/tests/test_deploy_linux_install.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
for rev in ("09fceb6", "1fad0a2"):
    for inject in (False, True):
        h = m.DeployInstallTests()
        h.setUp()
        try:
            h.text = subprocess.check_output(["git", "show", rev + ":scripts/deploy_linux.sh"]).decode()
            body = r'''#!/usr/bin/env bash
printf 'chmod %s\n' "$*" >> "$PROBE_DIR/stubs.log"
if [ "${FAIL_KEY_CHMOD:-0}" = 1 ] && [ "$1" = 600 ] && [[ "$2" = */data/registry.key ]]; then
  echo 'chmod stub: forced key permission failure' >&2
  exit 1
fi
exec "@REAL_CHMOD@" "$@"
'''.replace("@REAL_CHMOD@", m._posix_tool("chmod"))
            stub = h.probe / "bin/chmod"
            stub.write_text(body, encoding="utf-8", newline="\n")
            stub.chmod(0o755)
            rc, out = h._run_k10_install("", "1", "chmodcase",
                run_env={"FAIL_KEY_CHMOD": str(int(inject))}, models="make_upgrade_model\n")
            result = (h.probe / "chmodcase.out").read_text(encoding="utf-8")
            print(json.dumps({
                "rev": rev, "inject": inject, "driver_rc": rc,
                "install_rc": int(re.search(r"INSTALL_RC=(\d+)", out)[1]),
                "chmod_failed": "chmod stub: forced key permission failure" in result,
                "stop_called": "systemctl stop ak-registry-server.service" in h.stubs_log(),
                "recovery": "RECOVERY" in result,
                "units": {u: {"active": h.unit_state(u, "active"),
                    "image": "NEW" if h.unit_running_bin(u) == m.AK_CLI_STUB else "OLD"}
                    for u in ("ak-registry-server.service", "ak-stun-server.service")}
            }))
        finally:
            h.doCleanups()
'@ | python -
```

## 5. 文档与范围

`2d77eed` 确实纳入 K11 报告、交接 §27、状态表、STATUS 与 K10 历史归属；`1fad0a2` 确实只有哈希回填。本轮本地同步修正“双双通过”和“K11 仅本地”的残留，将下一步对准仍需修复的来源识别与权限失败传播。历史报告保留原判断，避免把原输入通过扩大成整个问题关闭。

Rust、依赖及 workflow 在修复区间无差异。ADR-001、无 relay／隐藏代理回退、原始字节不压缩不转码约束不变。固定 SHA 的 CI 平台门槛、本机 Rust 构建与隔离模型分别记录，不互相替代，也不计作物理双机验收。

历史归属补注（K13 复核）：本文及当轮本地同步随后由执行方在 `a5a5ad6` 提交，`0b41201` 回填哈希；上述“仅本地”描述的是出具时点。后续候选的判断见 [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md)，原发现与原证据保持。
