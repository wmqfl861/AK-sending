# 批次 K 修复轮十三独立复核（2026-11-15）

复核范围：`dev/batch-k-20261114` 的 `0b41201..943a346`；实现提交为 `b4b4ca1`，`943a346` 只在两个文档回填哈希。被审 tip 固定为 `943a346316b488449ca51d762f26b3f41f04d9f6`。结论为 **REJECTED**：原固定 cwd 字节夹具与生成脚本引号/美元路径夹具已翻转，但 cwd 校验的第二次读取失败仍可确认截断路径；另发现显式相对身份路径未经运行 cwd 解析便被当成迁移来源。

逐项状态只在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。命令、计数、GitHub 运行编号和证据边界集中在 [K 交接 §33](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#33-修复轮十三独立复核证据2026-11-15)。本轮只新增审查报告并同步本地文档，未修改被审代码，未提交、合并、推送 main 或删除批次分支。源码行号指固定 tip。

已使用当前 git 凭据直接查询 GitHub run/jobs API，从 in_progress 等到固定 SHA 的 completed/success，并核对所有 job 的 head_sha 与步骤结果。手动 release 深测的跳过不算执行。CI 成功不能消除下述独立反例。

## 1. K10-N1 / P1：cwd 字节比较的第二次读取失败时仍确认截断来源

稳定 readlink 输出下，尾 LF 会触发 `lost trailing bytes`，内嵌 LF 会触发 `cannot carry faithfully`；普通 cwd 控制正常。问题是候选路径与用于证明其字节完整性的计数来自两次读取，第二次读取没有成功性门槛。

`scripts/deploy_linux.sh:1331` 的第一次 `readlink` 位于普通命令替换中，仍会删除输出尾 LF。`:1338` 再读一次来取得工具字节数，`:1340` 只在计数大于零时比较。实际生成的 phase1 使用 `set -eu`，没有 pipefail：第二次 `readlink` 退出 1 且无 stdout 时，`wc -c` 输出 0 且管道成功。即使增加 pipefail，现有 `|| true` 也会屏蔽失败，零计数仍让检查跳过。

```bash
cwd_tool_bytes=$(sudo -n readlink /proc/$reg_pid/cwd 2>/dev/null | wc -c || true)
cwd_bash_bytes=$(printf '%s' "$reg_cwd" | wc -c)
if [ "$cwd_tool_bytes" -gt 0 ] \
   && [ "$((cwd_tool_bytes - 1))" != "$cwd_bash_bytes" ]; then
  ... # refusal
fi
```

保持 K13 原环境输入：空 USERPROFILE、非空 HOME、无显式 `--id-dir`。第一次 readlink 发出实际 cwd `/home/runtime-old\n` 加工具自己的终止换行；第二次仅返回 1、不输出字节，模拟校验读取时 proc 不可用。没有修改生产生成器、决策、compose 或安装代码。

| 输入 | phase1 / decision / install | 实际结果 |
|---|---|---|
| 普通 cwd，读取成功 | 0 / 0 / 0 | 选择普通运行目录，正常复制 |
| 尾 LF cwd，读取均成功 | 3 / 未执行 / 未执行 | 拒绝，没有 `runtime-id-dir` |
| 相同尾 LF cwd，仅第二次读取失败 | **0 / 0 / 0** | **确认 `/home/runtime-old/.ak-sending`，从不含 LF 的前缀目录复制 key/db** |

后一个模型的生成脚本实际含 `DATA_SOURCE=/home/runtime-old/.ak-sending`；日志有失败的第二次 cwd 查询及后续 stop→cp→cmp，目标实际为合成 `cwd-prefix-decoy-registry.key` / `cwd-prefix-decoy-registry.db`。这证明字节校验不可用被当成允许继续，原截断错源依旧可达。没有把 proc 消失交错声称为已发生的生产事故。

**修复要求：** 校验与解析应基于同一次成功读取的完整输出，任何读取失败、零字节或无法证明字节完整性的情况均应在停服前拒绝。不能用另一遍读取的失败作为跳过校验的条件。增加首次读取成功、校验读取失败的完整链拒绝回归，保留稳定尾/内 LF 和普通 cwd 控制。

## 2. K14-N1 / P1：显式相对 `--id-dir` 按部署 cwd 迁移，未绑定运行来源

这是新发现的既有缺口；相同输入在 `0b41201` 也复现，不归为 `b4b4ca1` 新引入。CLI 的 `crates/ak-cli/src/main.rs:709` 直接使用显式 `PathBuf`，允许相对路径。下列 unit 与逐参数 NUL cmdline 一致：

```ini
[Service]
User=otheruser
WorkingDirectory=/home/runtime-old
ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200 --id-dir state
```

实际运行 cwd 模型也是 `/home/runtime-old`，因此运行身份应在 `/home/runtime-old/state`。临时文件树同时建立该真实来源和部署脚本 cwd 下的 `state`，两者 key/db 内容不同。

`scripts/deploy_linux.sh:1232` 提取显式参数后，只有结果为空才进入环境/cwd 推导；非空相对 `state` 绕过 cwd 校验，`:1358` 直接报告 `runtime-id-dir: state`。`:2255` 将其作为 DATA_SOURCE_DIR，`:2261` 将其作为 REG_RW，新白名单允许 `state`。随后 `:1876`、`:1878` 以部署脚本的 cwd 执行真实 cp/cmp。

| 输入及版本 | phase1 / decision / install | 实际落盘 |
|---|---|---|
| `943a346`，`--id-dir state` | **0 / 0 / 0** | **目标 key/db 为部署 cwd 下 `state` 的 `deploy-cwd-decoy-*`** |
| `0b41201`，完全相同输入 | **0 / 0 / 0** | 同样错源，证明是既有遗漏 |
| `943a346`，仅把 unit+argv 改成 `--id-dir /home/runtime-old/state` | 0 / 0 / 0 | 目标为真实来源的 `runtime-real-*` |

相对路径两案都未查询 `/proc/4242/cwd`。`:1933`、`:1938` 生成的新 unit 保留 `--id-dir state` 与 `ReadWritePaths=state`，且没有 `WorkingDirectory=`。此处只把 install=0 记为 systemctl 隔离模型结果；未验证真实 systemd 如何处理相对 ReadWritePaths，也不声称 Linux 真实服务已经成功上线。生产 Shell 实际从错误目录复制、cmp 成功并写出丢失 cwd 语义的 unit，已足以否定身份来源保真。

**修复要求：** 所有显式相对身份路径都必须在迁移前解析到已验证的运行 cwd，并让新 unit 继续指向同一绝对来源；当前不能忠实处理的输入应在停服前明确拒绝。不能把“参数文字相同”或“字符适合 Shell 赋值”当作路径对象相同。回归应保留不同 cwd、真实来源和诱饵目录的完整链对照。

## 3. 原 K13 反例与提交内历史对照的证据边界

独立专项使用各版本真实 `git show` 源码，同一 Bash 执行内串联实际 decision→compose→write_phase456→install，确认当前的引号 `/home/oth'er'user` 与字面 `/home/$USER` 都在 decision 返回 1、没有 CHOSEN 输出和目标 key；普通路径完整链复制正确字节。`0b41201` 的引号输入则保留到生成脚本 `DATA_SOURCE=/home/oth'er'user/.ak-sending`，安装 Shell 去引号后实际复制另一目录。原 K13 修复的判断基于这条独立链。

提交内 `test_k13n1_round12_tip_quoted_path_copies_wrong_dir` 的安装段证据过宽：`:3616` 调用 `_run_k10_install`，后者 `:1132` 把路径直接串成 caller Shell 源码，`DATA_SOURCE_DIR=/home/oth'er'user/.ak-sending` 在调用生成器前已经去引号；实际生成的 DATA_SOURCE 已经是无引号路径。该 helper 还通过 `:1137 → :932 → :856` 使用当前 `self.text`，没有把 baseline 文本交给生成器。不过本轮两个版本的整个 write_phase456 块逐字相同，此来源错误没有额外行为差异。

因此该测试通过本身不能证明其注释所称的“原 baseline 生成脚本重新解释路径”；应改成让 baseline 决策值直接流入 baseline 生成器，并断言生成源码仍保留原字节。cwd baseline `:3562` 实际只执行 phase1，能证明“确认截断目录”，不能扩大为整个安装链。独立完整链已补证原引号缺陷；这些证据限定没有被冒充为新的生产 P1。

## 4. 可复现步骤

在固定候选仓库根目录用 PowerShell 执行以下代码。所有文件均为合成数据；只在临时 readlink stub 注入第二次读取失败，其余生产源码从对应 git 版本取得。runner 对拒绝、完整链退出与两份落盘字节均有断言，退出 0 表示反例和控制按报告复现。材料保留在输出的 TEMP 目录。

```powershell
@'
import importlib.util, json, re, subprocess, tempfile
from pathlib import Path
spec = importlib.util.spec_from_file_location(
    "deploy_tests", Path("scripts/tests/test_deploy_linux_install.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
outdir = Path(tempfile.mkdtemp(prefix="ak-k14-counterexamples-"))
cases = [
    ("cwd-stable-lf", "943a346", "cwd", False),
    ("cwd-second-read-fails", "943a346", "cwd", True),
    ("relative-current", "943a346", "relative", False),
    ("relative-baseline", "0b41201", "relative", False),
    ("absolute-control", "943a346", "absolute", False),
]
results = []
for label, rev, mode, fail_second in cases:
    h = m.DeployInstallTests()
    h.setUp()
    try:
        h.text = subprocess.check_output(
            ["git", "show", rev + ":scripts/deploy_linux.sh"]).decode("utf-8")
        if mode == "cwd":
            h._make_k13_cwd_fixture(b"/home/runtime-old\n")
            h.driver_env["PROC_CWD_RAW_FILE"] = (h.probe / "cwd-bytes").as_posix()
        else:
            identity = "state" if mode == "relative" else "/home/runtime-old/state"
            args = "--port 17200 --id-dir " + identity
            h.make_private_discovery_model(
                main_exec="/opt/ak-sending/ak-cli registry-server " + args,
                cmdline_extra=args,
                unit_service_lines=["User=otheruser", "WorkingDirectory=/home/runtime-old"],
                environ=b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0")
            h.driver_env["PROC_CWD_MAP"] = "/proc/4242/cwd=/home/runtime-old"
            for rel, tag in [("home/runtime-old/state", "runtime-real"),
                             ("state", "deploy-cwd-decoy")]:
                d = h.probe / rel
                d.mkdir(parents=True, exist_ok=True)
                for name in ("registry.key", "registry.db"):
                    (d / name).write_text(tag + "-" + name, encoding="utf-8")
        if fail_second:
            link = h.probe / "bin/readlink"
            marker = 'if [ -n "${PROC_EXE_MAP:-}" ]; then'
            inject = ('if [ "$target" = /proc/4242/cwd ]; then\n'
                '  if [ -f "$PROBE_DIR/cwd-read-once" ]; then\n'
                '    printf "review-cwd-second-read-failed\\n" >> "$PROBE_DIR/stubs.log"\n'
                '    exit 1\n  fi\n'
                '  : > "$PROBE_DIR/cwd-read-once"\nfi\n')
            source = link.read_text(encoding="utf-8")
            assert marker in source
            link.write_text(source.replace(marker, inject + marker, 1),
                            encoding="utf-8", newline="\n")
        st = h.probe / "systemctl-state"
        st.mkdir(exist_ok=True)
        for unit, pid in [("oldregistry.service", 4242), ("oldsignal.service", 4243)]:
            for suffix, value in [("enabled", "1"), ("active", "active"), ("pid", str(pid))]:
                (st / (unit + "." + suffix)).write_text(value)
            (st / (str(pid) + ".exe")).write_text("/opt/ak-sending/ak-cli")
        (h.probe / "etc/systemd/system/oldsignal.service").write_text(
            "[Service]\nExecStart=/opt/ak-sending/ak-cli signal-server\n")
        rc, out = h.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || exit 90\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        phase = (h.probe / "p1.out").read_text(encoding="utf-8")
        phase_rc = int(re.search(r"RUN_RC=(\d+)", out)[1])
        assert rc == 0, out
        result = {"case": label, "rev": rev, "phase1_rc": phase_rc,
            "runtime_id": [x for x in phase.splitlines() if "runtime-id-dir:" in x]}
        if mode == "cwd" and not fail_second:
            assert phase_rc == 3 and not result["runtime_id"], result
            assert "lost trailing bytes" in phase, phase
        else:
            assert phase_rc == 0, (result, phase)
            (h.probe / "phase1.log").write_text(phase, encoding="utf-8")
            body = 'WORK="$PROBE_DIR"\nKEEP_LEGACY_SIGNAL=0\nCHECK_ONLY=0\ndeploy_decision() {\n'
            body += h.decision_block(h.text, rev) + h.decisions_block(h.text, rev)
            body += '\n}\ndeploy_decision\necho "CHOSEN_DATA_SOURCE=$DATA_SOURCE_DIR"\n'
            body += ('TS=t12345 BUILD_DIR= PREBUILT_MODE=1 STAGING_DIR="$PROBE_DIR/staging" '
                'STAGED_BIN="$PROBE_DIR/staging/ak-cli" SHORT_SHA=deadbee CLOSE_7100=0 '
                'write_phase456 "$PROBE_DIR/actual456.sh" || exit 90\n')
            body += 'bash "$PROBE_DIR/actual456.sh" > "$PROBE_DIR/actual456.out" 2>&1; echo "INSTALL_RC=$?"\n'
            irc, iout = h.run_driver(h.definitions() + m.BOX_MODELS, body,
                                    name="actual-driver.sh", timeout=180)
            result.update(driver_rc=irc,
                chosen=[x for x in iout.splitlines() if x.startswith("CHOSEN_")],
                install_rc=int(re.search(r"INSTALL_RC=(\d+)", iout)[1]))
            assert irc == 0 and result["install_rc"] == 0, (iout, result)
            tag = ("cwd-prefix-decoy" if mode == "cwd" else
                   "deploy-cwd-decoy" if mode == "relative" else "runtime-real")
            result["data"] = {name: (h.probe / "opt-ak/data" / name).read_text()
                              for name in ("registry.key", "registry.db")}
            assert all(value == tag + "-" + name
                       for name, value in result["data"].items()), result
            log = h.stubs_log()
            if fail_second:
                assert "review-cwd-second-read-failed" in log, log
            if mode == "relative":
                assert "readlink /proc/4242/cwd" not in log, log
            (h.probe / "actual-driver.out").write_text(iout, encoding="utf-8")
        dst = outdir / label
        dst.mkdir()
        for name in ("p1.sh", "p1.out", "actual-driver.sh", "actual-driver.out",
                     "actual456.sh", "actual456.out", "stubs.log"):
            p = h.probe / name
            if p.exists(): (dst / name).write_bytes(p.read_bytes())
        unit = h.probe / "etc/systemd/system/ak-registry-server.service"
        if unit.exists(): (dst / "new-unit.txt").write_bytes(unit.read_bytes())
        (dst / "readlink-stub").write_bytes((h.probe / "bin/readlink").read_bytes())
        results.append(result)
        print(json.dumps(result), flush=True)
    finally:
        h.doCleanups()
(outdir / "results.json").write_text(json.dumps(results, indent=2), encoding="utf-8")
print("EVIDENCE_DIR=" + str(outdir))
'@ | python -
```

## 5. 文档与范围

`b4b4ca1` 已提交 K13 报告、交接 §31、STATUS、状态表、K12 历史归属与执行方 §32；`943a346` 只回填哈希。本轮补明 K13 同步“当时本地、随后入库”的历史归属，新增 K14 与交接 §33，并同步当前入口与状态表。文档归属更正不是拒绝依据。

Rust、依赖和 workflow 在修复区间无差异。固定 tip CI、本机门槛与隔离行为模型分别记录于交接。没有生产 SSH、部署、物理双机或抓包；cwd/proc/systemctl/权限是 Windows Git Bash 模型，Windows 未创建含 LF 的 Linux cwd。真实 Shell 执行、临时文件 cp/cmp 与 unit 写入不等于原生 Linux 部署验收。ADR-001、无 relay/隐藏代理回退及原始字节约束不变。
