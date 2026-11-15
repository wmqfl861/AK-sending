# 批次 K 修复轮十二独立复核（2026-11-15）

复核范围：`dev/batch-k-20261114` 的 `1fad0a2..0b41201`；实现提交为 `a5a5ad6`，`0b41201` 仅回填交接与状态表的哈希。被审 tip 固定为 `0b41201778ece5a9310bc97f24370444c39d3335`。结论为 **REJECTED**：原精确输入已翻转，但 K10-N1 的 cwd 路径仍会丢失字节并确认另一目录；另发现生成安装脚本时重新解释合法路径、迁移错误身份的既有缺口 K13-N1。

逐项状态只在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。命令、计数、GitHub 运行编号和证据边界集中在 [K 交接 §31](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#31-修复轮十二独立复核证据2026-11-15)。本轮只新增审查报告并同步本地文档，未修改被审代码，未提交、合并、推送 main 或删除批次分支。源码行号指固定 tip。

执行方对 K11 摘要的更正已接受：K11 通过的是 K10-N2 与 K10-N1 的原始输入，K10-N1 当时仍阻断。本轮不以已经纠正的摘要作为拒绝原因。已用当前 git 凭据直接查询 GitHub run/jobs API，从 in_progress 等待到固定 tip 的 completed/success，并核对各 job 的完整 head SHA 和步骤；手动 release 深测的跳过不算执行。

## 1. K10-N1 / P1：环境值检查有效，但运行 cwd 仍被截断后确认

当前 `grep -za -m1`、不可传输字节拒绝和字节数比较确实使原来的实际 USERPROFILE 值内 LF 输入拒绝，原 EXTRA 伪造变量输入也保持正确。空 USERPROFILE 的普通路径输入已改用 `/proc/<PID>/cwd`，配置与运行目录不同的原反例也选择运行目录。问题在于，新增的字节检查只覆盖 USERPROFILE/HOME，没有覆盖随后读取的 cwd。

`scripts/deploy_linux.sh:1315` 的普通命令替换会删除 readlink 输出的所有尾 LF，无法区分命令附加的行终止符与 cwd 本身的 LF。值内 LF 则保留到 `:1330` 的行制报告，再被 `:2097` 的 `sed ... | head -1` 截断。两种情况均未拒绝。

输入保持为空 USERPROFILE、非空 HOME、没有显式 `--id-dir` 的运行 registry；unit 配置为 `/home/configured-new`，运行 cwd 由 readlink 的精确字节决定：

| 实际 cwd（`\n` 表示 LF 字节） | 当前 phase1 / decision / install | 最终选择及落盘 |
|---|---|---|
| `/home/runtime-old`，正常控制 | 0 / 0 / 0 | `/home/runtime-old/.ak-sending`，符合默认链 |
| `/home/runtime-old\n`，LF 在值尾 | **0 / 0 / 0** | **`/home/runtime-old/.ak-sending`**；复制无 LF 前缀目录的合成 key/db |
| `/home/runtime-old\nsuffix`，值内 LF | **0 / 0 / 0** | **`/home/runtime-old`**；连后面的 `/.ak-sending` 也被逐行消费丢失，复制该前缀目录的合成 key/db |

CLI 的 `main.rs:709` 默认链对空 USERPROFILE 得到相对 `.ak-sending`；Linux 的实际路径解析应基于包含 LF 的 cwd。当前安装日志实际查询了 readlink，随后对错误来源执行 cp/cmp，并打印 `cmp-verified post-stop`；新 unit 的 `--id-dir` 指向已被填入错误字节的托管目录。这不是单纯日志显示问题。

现有 harness 的 `PROC_CWD_MAP` 自身按 LF 分记录，不能表达此输入。专项仅在临时 readlink stub 中定点返回 `/proc/4242/cwd` 的原始字节，生产生成器、决策、compose 和安装链均原样执行；集成者用下文脚本独立重放了值尾 LF。Windows 没有创建含 LF 的 Linux 目录，cwd/proc/systemctl 为模型；Bash 的截断和对前缀目录的实际复制、校验、unit 写入是执行结果，不能称为原生 Linux 部署验证。

**修复要求：** 将所有身份来源路径，包括 cwd，纳入同一套字节保真或明确拒绝规则。应在任何有损转换前取得完整字节并检查，不能在命令替换已删除尾 LF 后才验证。将最终来源、生成脚本与实际目标数据纳入回归，保留原环境值和普通 cwd 的控制。

## 2. K13-N1 / P1：生成脚本重新解释路径，报告正确来源却复制另一身份

这是本轮新发现的既有缺口，`1fad0a2` 相同输入也复现，不归为 `a5a5ad6` 新引入。`scripts/deploy_linux.sh:1523` 位于生成 phase456 的未引用 heredoc 内：

```bash
DATA_SOURCE=${DATA_SOURCE_DIR:-}
```

预检取得的路径直接成为下一份 Bash 脚本的源码，没有序列化为字面值。当前新增检查只拒绝 LF/空格/tab，合法路径中的引号、美元符号等仍通过；下一次执行会按 Shell 语法解释这些字节。

集成者使用真实 NUL 环境记录 `USERPROFILE=/home/oth'er'user`，unit 用 EnvironmentFile 双引号保存同一值。临时文件系统确实同时存在带引号的真实来源和不带引号的另一来源，数据各不相同：

| 环节 | 当前和 `1fad0a2` 的实际结果 |
|---|---|
| phase1 与 decision | 均成功；`CHOSEN_DATA_SOURCE=/home/oth'er'user/.ak-sending`，仍是原字节 |
| 生成脚本 | `DATA_SOURCE=/home/oth'er'user/.ak-sending` |
| Bash 执行该赋值 | 单引号成为语法，变量值变为 `/home/otheruser/.ak-sending` |
| 迁移与启用 | 安装返回 0；目标 key/db 是另一目录的 `private-key-bytes` / `private-db-bytes`，真实来源的 `runtime-real-*` 未迁入；新 unit 指向错误数据所在的托管目录 |

相同流程的普通路径控制复制正确字节。专项还独立交叉验证了字面 `USERPROFILE=/home/$USER`：预检和决策保留 `$USER`，但安装 shell 将其展开为部署环境的 `expanded-user`，从该目录复制合成 decoy 并返回成功。字面目录与展开后的目录均实际落盘，不依赖 LF 路径模型。

因此源路径在预检与安装之间不再相同，既有 cmp 只能证明复制结果等于被重解释后的错误来源。此处已证明身份/数据库错源；没有执行任意命令注入或声称已经造成生产泄露。

**修复要求：** 跨生成脚本边界把路径编码为不会再次解释的字面值，或在预检中明确拒绝当前无法忠实承载的完整输入集；仅给展开结果套双引号仍会重新解释美元展开。补充普通路径、引号及字面美元符号的完整链测试，断言运行来源、生成脚本执行后的来源和落盘字节一致，或在停服前明确拒绝。

## 3. 原权限反例与数据库夹具的实际结果

K12 报告的原 chmod runner 只增加当前版本后重放：`09fceb6` 在权限失败时停服前退出；`1fad0a2` 仍错误返回成功并运行新 image；当前在现存 key 的 chmod 失败后返回 5，经统一恢复使两服务 active 且运行旧 image。提交内回归另断言 `RECOVERY OK` 和旧磁盘二进制；成功和无 key 控制也实际执行，无 key 控制确实跳过该 chmod。对应实现为 `scripts/deploy_linux.sh:1873` 的存在分支与显式 `|| return 1`。

K11 晚提交回归现在显式建立旧服务 enabled/active/PID/运行 image，stop-entry hook 在提交 SQLite UPDATE 前断言服务仍 active。当前目标包含最终 `revoked / 2`，历史对照仍遗漏；原停止后迁移及复制/冲突失败恢复的证据保留。专项第一次并发负载中的该项发生 harness 超时，原样单项重跑成功，全量中也成功；这次超时与证据层级如实记录于交接，不隐去或伪报首轮全过。

## 4. 可复现步骤

在固定候选仓库根目录用 PowerShell 执行以下代码。生产生成器从真实 git 版本提取，运行真实 phase1、决策、compose、安装；所有文件都是临时合成数据，特权接口由提交内 harness 映射。仅 cwd 案的临时 readlink stub 补充原始字节返回，现有行制 map 无法表达该输入。代码包含对退出值及错误落盘字节的断言，退出 0 表示反例按报告复现；完整日志、生成脚本及新 unit 留在输出的 TEMP 目录。

```powershell
@'
import importlib.util, json, re, subprocess, tempfile
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "deploy_tests", Path("scripts/tests/test_deploy_linux_install.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
outdir = Path(tempfile.mkdtemp(prefix="ak-k13-counterexamples-"))
base = "/opt/ak-sending/ak-cli registry-server --port 17200"
cases = [
    ("cwd-tail-lf", "0b41201", "", "/home/runtime-old\n"),
    ("quoted-path", "0b41201", "/home/oth'er'user", None),
    ("quoted-baseline", "1fad0a2", "/home/oth'er'user", None),
]
for label, rev, profile, cwd in cases:
    h = m.DeployInstallTests()
    h.setUp()
    try:
        h.text = subprocess.check_output(
            ["git", "show", rev + ":scripts/deploy_linux.sh"]).decode("utf-8")
        h.make_private_discovery_model(main_exec=base,
            cmdline_extra="--port 17200",
            unit_service_lines=["User=otheruser",
                "WorkingDirectory=/home/configured-new",
                "EnvironmentFile=/etc/default/oldregistry"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=" + profile.encode()
                + b"\0USER=otheruser\0")
        ef = h.probe / "etc/default/oldregistry"
        ef.parent.mkdir(parents=True, exist_ok=True)
        ef.write_text('HOME=/home/otheruser\nUSERPROFILE="' + profile
            + '"\nUSER=otheruser\n', encoding="utf-8")
        rel, tag = (("home/runtime-old/.ak-sending", "cwd-prefix-decoy")
                    if cwd is not None else
                    (profile.lstrip("/") + "/.ak-sending", "runtime-real"))
        d = h.probe / rel
        d.mkdir(parents=True, exist_ok=True)
        for name in ("registry.key", "registry.db"):
            (d / name).write_text(tag + "-" + name, encoding="utf-8")
        if cwd is not None:
            # PROC_CWD_MAP is itself line-based. Replace only this proc read
            # in the temporary stub so it emits the exact Linux readlink bytes.
            h.driver_env["REVIEW_CWD_BYTES"] = cwd
            link = h.probe / "bin/readlink"
            marker = 'if [ -n "${PROC_EXE_MAP:-}" ]; then'
            inject = ('if [ "$target" = /proc/4242/cwd ]; then\n'
                '  printf \'%s\\n\' "$REVIEW_CWD_BYTES"\n  exit 0\nfi\n')
            text = link.read_text(encoding="utf-8")
            assert marker in text
            link.write_text(text.replace(marker, inject + marker, 1),
                            encoding="utf-8", newline="\n")
        st = h.probe / "systemctl-state"
        st.mkdir(exist_ok=True)
        for unit, pid in (("oldregistry.service", 4242), ("oldsignal.service", 4243)):
            for suffix, val in (("enabled", "1"), ("active", "active"), ("pid", str(pid))):
                (st / (unit + "." + suffix)).write_text(val)
            (st / (str(pid) + ".exe")).write_text("/opt/ak-sending/ak-cli")
        (h.probe / "etc/systemd/system/oldsignal.service").write_text(
            "[Service]\nExecStart=/opt/ak-sending/ak-cli signal-server\n")
        rc, out = h.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || exit 90\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        phase = (h.probe / "p1.out").read_text(encoding="utf-8")
        drc, decision = h.run_decision_and_config(phase, "decision.sh")
        body = 'WORK="$PROBE_DIR"\nKEEP_LEGACY_SIGNAL=0\nCHECK_ONLY=0\ndeploy_decision() {\n'
        body += h.decision_block(h.text, rev) + h.decisions_block(h.text, rev)
        body += '\n}\ndeploy_decision\n'
        body += ('TS=t12345 BUILD_DIR= PREBUILT_MODE=1 STAGING_DIR="$PROBE_DIR/staging" '
            'STAGED_BIN="$PROBE_DIR/staging/ak-cli" SHORT_SHA=deadbee CLOSE_7100=0 '
            'write_phase456 "$PROBE_DIR/actual456.sh" || exit 90\n')
        body += 'bash "$PROBE_DIR/actual456.sh" > "$PROBE_DIR/actual456.out" 2>&1; echo "INSTALL_RC=$?"\n'
        irc, iout = h.run_driver(h.definitions() + m.BOX_MODELS, body,
                                name="actual-driver.sh", timeout=120)
        dst = h.probe / "opt-ak/data"
        data = {name: (dst / name).read_text() if (dst / name).exists() else None
                for name in ("registry.key", "registry.db")}
        result = {"case": label, "rev": rev, "cwd": cwd,
            "phase_driver_rc": rc, "phase_rc": re.search(r"RUN_RC=(\d+)", out)[1],
            "decision_rc": drc, "chosen": [l for l in decision.splitlines()
                if l.startswith("CHOSEN_DATA_SOURCE=")], "install_driver_rc": irc,
            "install_rc": re.search(r"INSTALL_RC=(\d+)", iout)[1], "data": data}
        assert (rc, drc, irc) == (0, 0, 0), (out, decision, iout)
        assert result["phase_rc"] == result["install_rc"] == "0", result
        assert data["registry.key"] == ("cwd-prefix-decoy-registry.key"
            if cwd is not None else "private-key-bytes"), result
        for name in ("p1.sh", "p1.out", "actual456.sh", "actual456.out", "stubs.log"):
            (outdir / (label + "-" + name)).write_bytes((h.probe / name).read_bytes())
        (outdir / (label + "-unit.txt")).write_bytes(
            (h.probe / "etc/systemd/system/ak-registry-server.service").read_bytes())
        (outdir / (label + ".json")).write_text(json.dumps(result, indent=2), encoding="utf-8")
        print(json.dumps(result), flush=True)
    finally:
        h.doCleanups()
print("EVIDENCE_DIR=" + str(outdir))
'@ | python -
```

## 5. 文档与范围

`a5a5ad6` 已提交 K12 报告、交接 §29、STATUS、状态表及 K11 历史归属，并增加执行方 §30；`0b41201` 仅回填哈希。本轮同步新增 K13、交接 §31 和当前状态，补明 K12 文档随后入库的历史归属。已接受的 K11 摘要更正不重新列为问题。

Rust、依赖与 workflow 在修复区间无差异。固定 SHA 的 CI、本机门槛和隔离行为模型分别记录；没有连接生产服务、执行远端 SSH、Linux 原生 systemd 部署、物理双机或抓包。ADR-001、无 relay/隐藏代理回退和原始字节约束保持；本审查不扩大为生产环境或任意数据库崩溃恢复验收。

归属补注（K14 复核，2026-11-15）：本报告及交接 §31、STATUS、状态表、K12 历史归属在出具时只保留本地，随后由执行方在 `b4b4ca1` 一并提交。本文“仅本地”指原出具时点；当前复核见 [K14](REVIEW_2026-11-15-batchK14.zh-CN.md)。
