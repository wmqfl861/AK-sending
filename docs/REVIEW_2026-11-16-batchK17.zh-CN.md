# 批次 K 修复轮十六独立复核（第 17 轮，2026-11-16）

历史归属补注（2026-11-16）：本报告及出具时的本地同步随后由执行方在 `063b80a` 入库，`4db2b4f` 仅回填该修复提交号。本页保留当时的历史裁决；新候选与同提交 CI 重跑的复核见 [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md)。

复核范围：`dev/batch-k-20261114` 的 `0602f92..7a93314`；代码与文档提交为 `f2574cb`，`7a93314` 只在交接回填一行提交号。固定 tip 为 `7a93314db81cabb0c089451d92eae1eb1d4f8d23`。结论为 **REJECTED**：读取者退出码修复及原始 grep、方括号输入均已翻转，但第二次 grep 提取失败仍可触发 HOME 错源迁移，新数据库切分还会破坏合法身份参数。固定 tip CI 已等到完成，结果为 failure。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。实际命令、运行编号、计数、仓外材料、权限错误与未执行项只记在 [K 交接 §39](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#39-修复轮十六独立复核证据2026-11-16)。本轮仅新增报告和同步本地文档，不改被审实现、不创建提交、不合并、不推送、不删除分支。下列源码行号均指固定 tip。

## 1. K10-N1 / P1 余项：存在性成功不保证后续提取成功，错误仍被当成缺失

`scripts/deploy_linux.sh:1314–1326` 新增的存在性检查本身以 `grep ... || rc=$?` 保护 errexit，能正确区分匹配、正常缺失与执行错误。原 K16 持续报错夹具现在在这里拒绝；健康、真缺失和存在但为空的控制均保持正确。

但随后 `:1327–1328` 再执行一次 grep，仍吞掉其错误：

```bash
ev_up_rec=$(grep -za -m1 '^USERPROFILE=' "$reg_env_raw" || true)
ev_home_rec=$(grep -za -m1 '^HOME=' "$reg_env_raw" || true)
```

`:1338` 的 `[ -n "$re_val" ] || continue` 没有检查前面保存的存在性 rc。故“空值 continue 仅跟随 triage 后的 rc=1”不符合实际控制流：第一次 rc=0 后也能走该 continue。

独立输入保持完整、健康的同一 environ 快照：`HOME=/home/otheruser`、`USERPROFILE=/home/runtime-real`、`USER=otheruser`，按 NUL 分隔。unit 使用 EnvironmentFile，没有显式 `--id-dir`。只让 USERPROFILE 的第一次存在性 grep 正常透传并记录 rc=0，第二次值提取返回 2、无 stdout；其他命令和快照内容均不变。

实际生成 phase1、真实 decision、生成安装脚本与离线 install 全部返回零。日志同时保留 `REVIEW_PROFILE_GREP_1_RC=0` 和 `REVIEW_PROFILE_GREP_2_ERROR=2`；报告身份目录却是 `/home/otheruser/.ak-sending`，实际目标 key/db 字节为 `fallback-decoy-*`。补充显式 enabled/active/PID/running 状态的模型仍得到相同结果。健康控制复制 `runtime-real-*`；原 K16 首次就报错的形态在当前拒绝，在 `0602f92` 仍复制 HOME 诱饵。

另一个辅助控制让第二次 grep 输出完整记录后返回非零，当前仍放行；该案选中正确来源，只证明错误状态未被处理，不将它描述为错源。第一次匹配、第二次人为返回无匹配的矛盾状态也放行；它只是状态一致性控制，不把正常 rc=1 称为工具错误。

此项是 K16 提取错误缺口未完整关闭：新加的前置读取拦住了持续故障，却没有核验实际消费的那次提取结果。不是读取者 PIPESTATUS 修复失败。

**修复要求：** 在产生实际记录值的同一次提取中捕获并分流 rc，只有确定无匹配才能进入 absent 默认链；工具错误无论 stdout 是否为空都拒绝。若保留多次提取，必须检查每次成功状态和存在性一致性。回归需包含“存在性成功、随后提取失败”，不能只让所有 grep 从第一次起持续报错。

## 2. K15-N1 / P1 新回归：按子串切分会改坏身份目录，结果校验仍放行

原 `db[1].sqlite` 相对路径现在被白名单在决策阶段拒绝，未调用 stop/install；普通相对名、等号参数、`./` 前缀和绝对路径控制保持原对象。原 `0602f92` 方括号形态仍静默保留相对 DB。这些有效修复不覆盖新切分的参数边界。

`scripts/deploy_linux.sh:2534–2535` 按整个 ExecStart 中最先出现的 `--db-path` 子串切分，不要求独立选项边界。下面是白名单内的合法输入，unit 与逐参数 NUL cmdline 一致，运行 cwd 为 `/home/runtime-old`：

```text
--port 17200 --id-dir /home/otheruser/--db-path-archive/.ak-sending --db-path state/registry.db
```

此时 `re_prefix` 在身份目录内部截断，`re_suffix` 从 `-archive/...` 开始，`:2536` 得到空 `re_sep`，`:2538` 没有移除任何实际 DB 参数。拼接后的真实 unit 变成：

```text
--id-dir /home/otheruser/--db-path/home/runtime-old/state/registry.db-archive/.ak-sending --db-path state/registry.db
```

身份参数被损坏，真正的数据库参数仍为相对路径。`:2542–2546` 只检查刚刚插入的 `--db-path${re_sep}${abs_db}` 子串，因此必然在坏身份目录中命中；它既没有核验真正的 DB 参数，也没有发现身份目录改变。

主复核与专项分别实际执行固定版本的 phase1、开启 errexit 的 decision、生成器及离线 install，全部返回零，日志仍声称已改写。`0602f92` 相同输入完整链正常，身份参数保持原值，真实 DB 参数变为 `/home/runtime-old/state/registry.db`；所以这是 `f2574cb` 引入的回归。等号参数形态也复现。

补充模型将 DB 改成 `home/otheruser/--db-path-archive/.ak-sending/review.sqlite`，并显式初始化旧服务 enabled/active/PID/running 状态。两版实际捕获 `oldregistry:exists:enabled:active`；当前仍损坏参数，旧版保持正确。新默认 cwd `/` 下的数据库诱饵位于实际生成 ReadWritePaths 的首个目录内。按生成 DB 参数读取合成 SQLite，当前选中 `new-cwd-decoy`，旧版选中 `runtime-real`。

**边界：** 上述 SQLite 查询验证路径指向，安装成功来自离线 systemctl 模型。损坏后的身份目录在可写范围外；本轮没有启动原生 systemd 或 registry CLI，不能据此声称真实服务已成功启动并使用错库。足以阻断的事实是停服调用前未拒绝、生成身份/数据库参数错误、离线安装报告成功。普通 `/state` 诱饵在 RWP 外的初始模型与可写范围内的补充模型分别保留。

**修复要求：** 按完整选项及其字面参数边界改写，验证最终解析出的 DB 参数等于已解析绝对路径，并验证身份参数未被改变。无法忠实解析时在停服前拒绝。不能以命令字符串包含刚刚拼入的子串作为改写成功证明；保留路径内部含选项字样、空格/等号形式、普通/方括号路径的当前与基线控制。

## 3. 读取者修复与 CI 的独立边界

生成代码的 environ 捕获及三条 cwd 捕获后均立即读取 `PIPESTATUS[0]`，没有中间命令覆盖状态。当前 cat 首条或完整输出后失败、三个 readlink 分支完整输出后失败、空输出、仅工具换行和真实尾 LF 均在 phase1 拒绝；tee 部分写后失败仍由 `set -eu` 终止。健康控制保持正确。`0602f92` 的同形非零读取仍被放行，部分 environ 仍复制 HOME 诱饵。生成脚本及实际行为均已核验，不能因 §1 的另一缺口否认本项修复。

使用当前 git 凭据直接查询 GitHub run/jobs API，从 in_progress 等到固定 tip 的 completed/failure，并核验完整 SHA、分支与 jobs/steps。失败定位在 Linux Rust 的 `Tests (workspace)`，annotations 报 exit 101；后续该 job 的 SQLite、持久化和 rustdoc 步骤跳过。其余常规 jobs 成功；手动 release-deep 的 skipped 不计为执行。

日志下载接口跳转到日志存储后返回 HTTP 401，已保留接口与错误，没有切换身份、重试被拒日志下载或绕过限制。同一凭据可访问的 annotations 只给退出码，没有具体失败测试；故不猜测根因、不称其为偶发失败，也不把本机 Windows 成功替代 Linux CI。完整复核不能认定 tip CI 通过。

本机常规 Windows Rust 门槛成功，release 深测另有失败：`random_interruptions_resume_to_identical_trees_across_10_seeds` 在 `crates/ak-itests/tests/property_random.rs:234`、seed=1 收到 `Err(IdleTimeout(30s))`，不满足该断言列举的错误种类，命令以 101 结束。原始失败保留、未重跑；Cargo 停止后续测试，不能称 release 全集通过。Rust 在本次区间无差异，故作为额外门槛结果记录，不归因于部署改动，也不假设与 Linux CI 失败为同一原因。实际执行、框架计数与自行跳过分别见交接。

## 4. 重放方式与验证层级

下面代码从仓库根目录执行，分别保存为仓外 `review_grep.py` 与 `review_db_core.py`。输出目录必须选新的 TEMP 子目录。脚本读取固定 git 版本的真实生成器和决策，修改的只有临时 OS/工具 stubs；不会调用 SSH、访问生产或把合成 key/db 写入仓库。退出零表示发现和控制符合断言，不表示产品通过。

Shell 生成和执行、cp/cmp、SQLite 文件操作是实际本机操作；proc、sudo、systemctl 和权限是 Windows Bash 离线模型。本轮没有生产访问、Linux 原生部署、物理双机或抓包。ADR-001、无 relay、无强推和凭据不入库约束保持。所有命令与完整门槛结果见交接证据表。

### 4.1 存在性与提取结果分离

```powershell
python <TEMP目录>/review_grep.py healthy presence-error prior-error-baseline extraction-error --active --output <新的TEMP输出目录>
```

```python
import importlib.util, json, re, shutil, subprocess, sys
from pathlib import Path

ROOT = Path.cwd()
args = sys.argv[1:]
OUT = Path(__file__).resolve().parent / 'grep'
if '--output' in args:
    index = args.index('--output')
    OUT = Path(args[index + 1]).resolve()
    del args[index:index + 2]
ACTIVE = '--active' in args
if ACTIVE:
    args.remove('--active')
OUT.mkdir(exist_ok=True)
spec = importlib.util.spec_from_file_location('deploy_tests', ROOT / 'scripts/tests/test_deploy_linux_install.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

CASES = [
    ('healthy', '7a93314', 'present', 0, 0, False, 'runtime-real'),
    ('absent', '7a93314', 'absent', 0, 0, False, 'fallback-decoy'),
    ('empty', '7a93314', 'empty', 0, 0, False, 'cwd-real'),
    ('presence-error', '7a93314', 'present', 1, 2, False, 'refuse'),
    ('prior-error-baseline', '0602f92', 'present', 1, 2, False, 'fallback-decoy'),
    ('extraction-error', '7a93314', 'present', 2, 2, False, 'fallback-decoy'),
    ('extraction-no-match', '7a93314', 'present', 2, 1, False, 'fallback-decoy'),
    ('extraction-full-error', '7a93314', 'present', 2, 2, True, 'runtime-real'),
]
if args:
    CASES = [x for x in CASES if x[0] in args]
results = []
for label, rev, profile_kind, fail_at, fail_rc, emit, expected in CASES:
    h = m.DeployInstallTests()
    h.setUp()
    try:
        h.text = subprocess.check_output(['git', 'show', rev + ':scripts/deploy_linux.sh']).decode('utf-8')
        env = b'HOME=/home/otheruser\0'
        if profile_kind != 'absent':
            env += b'USERPROFILE=' + (b'/home/runtime-real' if profile_kind == 'present' else b'') + b'\0'
        env += b'USER=otheruser\0'
        h.make_private_discovery_model(main_exec=h.K9_BASE, cmdline_extra='--port 17200',
            unit_service_lines=['User=otheruser', 'EnvironmentFile=/etc/default/oldregistry'], environ=env)
        envfile = h.probe / 'etc/default/oldregistry'
        envfile.parent.mkdir(parents=True, exist_ok=True)
        envfile.write_bytes(env.replace(b'\0', b'\n'))
        for path, marker in [('home/runtime-real/.ak-sending', 'runtime-real'),
                             ('home/otheruser/.ak-sending', 'fallback-decoy'),
                             ('home/runtime-old/.ak-sending', 'cwd-real')]:
            d = h.probe / path
            d.mkdir(parents=True, exist_ok=True)
            for name in ('registry.key', 'registry.db'):
                (d / name).write_text(marker + '-' + name, encoding='utf-8')
        h.driver_env.update(REVIEW_FAIL_AT=str(fail_at), REVIEW_FAIL_RC=str(fail_rc),
                            REVIEW_EMIT=str(int(emit)), PROC_CWD_MAP='/proc/4242/cwd=/home/runtime-old')
        stub = h.probe / 'bin/grep'
        stub.write_text(r'''#!/usr/bin/env bash
set -u
if [ "${1:-}" = -za ] && [ "${2:-}" = -m1 ] && [ "${3:-}" = '^USERPROFILE=' ]; then
  n=0
  [ ! -f "$PROBE_DIR/profile-grep-count" ] || read -r n < "$PROBE_DIR/profile-grep-count"
  n=$((n + 1))
  printf '%s\n' "$n" > "$PROBE_DIR/profile-grep-count"
  if [ "$n" = "$REVIEW_FAIL_AT" ]; then
    printf 'REVIEW_PROFILE_GREP_%s_ERROR=%s\n' "$n" "$REVIEW_FAIL_RC" >> "$PROBE_DIR/stubs.log"
    if [ "$REVIEW_EMIT" = 1 ]; then @GREP@ "$@"; fi
    exit "$REVIEW_FAIL_RC"
  fi
  @GREP@ "$@"
  rc=$?
  printf 'REVIEW_PROFILE_GREP_%s_RC=%s\n' "$n" "$rc" >> "$PROBE_DIR/stubs.log"
  exit "$rc"
fi
exec @GREP@ "$@"
'''.replace('@GREP@', m._posix_tool('grep')), encoding='utf-8', newline='\n')
        stub.chmod(0o755)
        body = r'''
WORK="$PROBE_DIR"
KEEP_LEGACY_SIGNAL=0
CHECK_ONLY=0
write_phase1 "$PROBE_DIR/phase1.sh" || exit 90
bash "$PROBE_DIR/phase1.sh" > "$PROBE_DIR/phase1.log" 2>&1
phase_rc=$?
printf 'PHASE1_RC=%s\n' "$phase_rc"
[ "$phase_rc" = 0 ] || exit 0
review_decision() {
''' + h.decision_block(h.text, rev) + h.decisions_block(h.text, rev) + r'''
printf 'DECISION_COMPLETED=1\n'
}
review_decision
printf 'DECISION_RC=%s\n' "$?"
printf 'CHOSEN_EXEC=%s\n' "$REGISTRY_EXEC"
printf 'CHOSEN_DATA_SOURCE=%s\n' "$DATA_SOURCE_DIR"
printf 'CHOSEN_RW=%s\n' "$REG_RW"
mkdir -p "$PROBE_DIR/staging"
cp "$AK_CLI_STUB" "$PROBE_DIR/staging/ak-cli"
chmod +x "$PROBE_DIR/staging/ak-cli"
TS=t12345
BUILD_DIR=
PREBUILT_MODE=1
STAGING_DIR="$PROBE_DIR/staging"
STAGED_BIN="$PROBE_DIR/staging/ak-cli"
SHORT_SHA=deadbee
CLOSE_7100=0
write_phase456 "$PROBE_DIR/phase456.sh" || exit 91
bash "$PROBE_DIR/phase456.sh" > "$PROBE_DIR/phase456.log" 2>&1
printf 'INSTALL_RC=%s\n' "$?"
'''
        if ACTIVE:
            body = r'''
printf '1\n' > "$ST/oldregistry.service.enabled"
printf 'active\n' > "$ST/oldregistry.service.active"
printf '4242\n' > "$ST/oldregistry.service.pid"
printf '%s\n' "$REMOTE_BIN" > "$ST/4242.exe"
printf '%s\n' "$REMOTE_BIN" > "$ST/oldregistry.service.running"
cp "$PROBE_DIR/ak-cli.oldstub" "$ST/oldregistry.service.running.bin"
printf 'REVIEW_OLD_ACTIVE_BEFORE=active\n' >> "$PROBE_DIR/stubs.log"
''' + body
        driver_rc, stdout = h.run_driver(h.definitions() + m.BOX_MODELS, body, timeout=180)
        (h.probe / 'driver.out').write_text(stdout, encoding='utf-8')
        phase = (h.probe / 'phase1.log').read_text(encoding='utf-8')
        log = h.stubs_log()
        phase_rc = int(re.search(r'^PHASE1_RC=(\d+)$', stdout, re.M).group(1))
        row = dict(label=label, rev=rev, driver_rc=driver_rc, phase1_rc=phase_rc,
                   markers=re.findall(r'^(?:REVIEW_|ENV_).*$', log, re.M),
                   reported_id=re.findall(r'^  runtime-id-dir: (.*)$', phase, re.M),
                   stopped='systemctl stop ' in log, active_model=ACTIVE,
                   errors=re.findall(r'^ERROR:.*$', phase, re.M))
        assert driver_rc == 0, stdout
        if expected == 'refuse':
            assert phase_rc == 3 and not row['stopped'], row
            assert 'cannot determine the presence of USERPROFILE' in phase, phase
            assert not (h.probe / 'opt-ak/data/registry.key').exists()
        else:
            assert phase_rc == 0 and 'DECISION_RC=0' in stdout and 'INSTALL_RC=0' in stdout, stdout
            copied = {n: (h.probe / 'opt-ak/data' / n).read_text(encoding='utf-8') for n in ('registry.key', 'registry.db')}
            assert all(v == expected + '-' + n for n, v in copied.items()), copied
            row.update(decision_rc=0, install_rc=0, copied=copied)
        if fail_at == 2:
            assert 'REVIEW_PROFILE_GREP_1_RC=0' in log, log
            assert f'REVIEW_PROFILE_GREP_2_ERROR={fail_rc}' in log, log
        dest = OUT / label
        assert not dest.exists(), dest
        shutil.copytree(h.probe, dest)
        row['evidence'] = str(dest)
        results.append(row)
        (OUT / 'results.json').write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps(row, ensure_ascii=False), flush=True)
    finally:
        h.doCleanups()
```

### 4.2 数据库参数边界及基线

```powershell
python <TEMP目录>/review_db_core.py substring-id-contained-active-tip substring-id-contained-active-baseline --output <另一个新的TEMP输出目录>
```

```python
import importlib.util,json,re,shlex,shutil,sqlite3,subprocess,sys,time
from pathlib import Path
ROOT=Path.cwd()
args_input=sys.argv[1:]
OUT=Path(__file__).parent
if '--output' in args_input:
 ix=args_input.index('--output')
 OUT=Path(args_input[ix+1]).resolve()
 del args_input[ix:ix+2]
OUT.mkdir(parents=True,exist_ok=True)
spec=importlib.util.spec_from_file_location('deploy_tests',ROOT/'scripts/tests/test_deploy_linux_install.py')
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
CASES=[
 ('substring-id-tip','7a93314','/home/otheruser/--db-path-archive/.ak-sending','state/registry.db',' '),
 ('substring-id-baseline','0602f92','/home/otheruser/--db-path-archive/.ak-sending','state/registry.db',' '),
 ('substring-id-contained-active-tip','7a93314','/home/otheruser/--db-path-archive/.ak-sending','home/otheruser/--db-path-archive/.ak-sending/review.sqlite',' '),
 ('substring-id-contained-active-baseline','0602f92','/home/otheruser/--db-path-archive/.ak-sending','home/otheruser/--db-path-archive/.ak-sending/review.sqlite',' '),
]
if args_input:
 CASES=[c for c in CASES if c[0] in args_input]
def token_value(line,key):
 argv=shlex.split(line)
 for i,x in enumerate(argv):
  if x==key:return argv[i+1]
  if x.startswith(key+'='):return x[len(key)+1:]
 return None
results=[];start=time.monotonic()
for label,rev,identity,dbarg,sep in CASES:
 h=m.DeployInstallTests();h.setUp()
 try:
  h.text=subprocess.check_output(['git','show',rev+':scripts/deploy_linux.sh']).decode('utf8')
  args='--port 17200 --id-dir'+sep+identity+' --db-path'+sep+dbarg
  h.make_private_discovery_model(main_exec='/opt/ak-sending/ak-cli registry-server '+args,cmdline_extra=args,unit_service_lines=['User=otheruser','WorkingDirectory=/home/runtime-old'],environ=b'HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0')
  live=h.probe/identity.lstrip('/');live.mkdir(parents=True,exist_ok=True)
  (live/'registry.key').write_text('synthetic-runtime-key',encoding='utf8')
  (live/'registry.db').write_text('synthetic-identity-default-db',encoding='utf8')
  live_db=dbarg if dbarg.startswith('/') else '/home/runtime-old/'+dbarg
  alt_db=None if dbarg.startswith('/') else '/'+dbarg
  for obj,marker in [(live_db,'runtime-real'),(alt_db,'new-cwd-decoy')]:
   if obj is None:continue
   path=h.probe/obj.lstrip('/');path.parent.mkdir(parents=True,exist_ok=True)
   if path.exists():path.unlink()
   con=sqlite3.connect(path);con.execute('CREATE TABLE review_marker(value TEXT)');con.execute('INSERT INTO review_marker VALUES (?)',(marker,));con.commit();con.close()
  if '-active-' in label:
   # Match the ss/proc listener model with actual stateful systemctl state.
   state=h.probe/'systemctl-state';state.mkdir(parents=True,exist_ok=True)
   for suffix,value in [('enabled','1'),('active','active'),('pid','4242')]:
    (state/('oldregistry.service.'+suffix)).write_text(value+'\n',encoding='utf8')
   (state/'4242.exe').write_text('/opt/ak-sending/ak-cli\n',encoding='utf8')
   (state/'oldregistry.service.running').write_text('/opt/ak-sending/ak-cli\n',encoding='utf8')
   (state/'oldregistry.service.running.bin').write_text(m.AK_CLI_OLD,encoding='utf8')
  h.driver_env['PROC_CWD_MAP']='/proc/4242/cwd=/home/runtime-old'
  body=r'''
WORK="$PROBE_DIR"
KEEP_LEGACY_SIGNAL=0
CHECK_ONLY=0
write_phase1 "$PROBE_DIR/phase1.sh" || exit 90
bash "$PROBE_DIR/phase1.sh" > "$PROBE_DIR/phase1.log" 2>&1
phase_rc=$?
printf 'PHASE1_RC=%s\n' "$phase_rc"
[ "$phase_rc" = 0 ] || exit 0
set -e
review_decision() {
'''+h.decision_block(h.text,rev)+h.decisions_block(h.text,rev)+r'''
printf 'DECISION_COMPLETED=1\n'
}
review_decision
printf 'DECISION_RC=%s\n' "$?"
printf 'CHOSEN_EXEC=%s\n' "$REGISTRY_EXEC"
printf 'CHOSEN_DATA_SOURCE=%s\n' "$DATA_SOURCE_DIR"
printf 'CHOSEN_RW=%s\n' "$REG_RW"
printf 'CHOSEN_PREOWNED=%s\n' "$REMOTE_DATA_PREOWNED"
mkdir -p "$PROBE_DIR/staging"
cp "$AK_CLI_STUB" "$PROBE_DIR/staging/ak-cli"
chmod +x "$PROBE_DIR/staging/ak-cli"
TS=t12345
BUILD_DIR=
PREBUILT_MODE=1
STAGING_DIR="$PROBE_DIR/staging"
STAGED_BIN="$PROBE_DIR/staging/ak-cli"
SHORT_SHA=deadbee
CLOSE_7100=0
write_phase456 "$PROBE_DIR/phase456.sh" || exit 91
set +e
bash "$PROBE_DIR/phase456.sh" > "$PROBE_DIR/phase456.log" 2>&1
printf 'INSTALL_RC=%s\n' "$?"
'''
  driver_rc,stdout=h.run_driver(h.definitions()+m.BOX_MODELS,body,name='driver.sh',timeout=180)
  (h.probe/'driver.out').write_text(stdout,encoding='utf8')
  p1=(h.probe/'phase1.log').read_text(encoding='utf8');log=h.stubs_log()
  rec={'case':label,'revision':rev,'input_identity':identity,'input_db':dbarg,'driver_rc':driver_rc,'phase1_rc':int(re.search('PHASE1_RC=(\\d+)',stdout)[1]),'runtime_fields':[x for x in p1.splitlines() if re.match(r'  runtime-',x)],'stopped':bool(re.search(r'^systemctl stop ',log,re.M))}
  dec=re.search('DECISION_RC=(\\d+)',stdout);ins=re.search('INSTALL_RC=(\\d+)',stdout)
  if dec:rec['decision_rc']=int(dec[1])
  if ins:rec['install_rc']=int(ins[1])
  rec['chosen']=dict(re.findall(r'^CHOSEN_(\w+)=(.*)$',stdout,re.M))
  install_log=h.probe/'phase456.log'
  if install_log.exists():
   rec['captured_old_state_active']='oldregistry:exists:enabled:active' in install_log.read_text(encoding='utf8')
  rec['refusal_lines']=[x for x in stdout.splitlines() if 'FAIL:' in x]
  unit=h.probe/'etc/systemd/system/ak-registry-server.service'
  if unit.exists():
   u=unit.read_text(encoding='utf8');rec['unit_fields']=[x for x in u.splitlines() if x.startswith(('ExecStart=','ReadWritePaths=','User=','WorkingDirectory='))]
   ex=re.search(r'^ExecStart=(.*)$',u,re.M)[1]
   db=token_value(ex,'--db-path');ident=token_value(ex,'--id-dir')
   rec['generated_db_arg']=db;rec['generated_identity_arg']=ident
   obj=h.probe/db.lstrip('/')
   con=sqlite3.connect(obj);rec['selected_db_marker']=con.execute('SELECT value FROM review_marker').fetchone()[0];con.close()
  dest=OUT/label;shutil.copytree(h.probe,dest,dirs_exist_ok=True)
  (dest/'result.json').write_text(json.dumps(rec,indent=2),encoding='utf8')
  results.append(rec);print(json.dumps(rec),flush=True)
 finally:h.doCleanups()
(OUT/('results-'+ '-'.join(c[0] for c in CASES[:2])+'.json')).write_text(json.dumps(results,indent=2),encoding='utf8')
print('ELAPSED_SECONDS='+str(time.monotonic()-start),flush=True)
for r in results:
 if '-active-' in r['case']:assert r['captured_old_state_active'],r
 if r['case']=='bracket-tip':
  assert r['phase1_rc']==0 and r['driver_rc']==1 and 'install_rc' not in r and not r['stopped'],r
  assert 'cannot bake faithfully' in '\n'.join(r['refusal_lines']),r
 elif r['case']=='bracket-baseline':
  assert r['phase1_rc']==r['decision_rc']==r['install_rc']==0 and r['generated_db_arg']==r['input_db'] and r['selected_db_marker']=='new-cwd-decoy',r
 elif r['case'].startswith('substring-id') and r['revision']=='7a93314':
  assert r['phase1_rc']==r['decision_rc']==r['install_rc']==0 and r['generated_db_arg']==r['input_db'] and r['generated_identity_arg']!=r['input_identity'] and r['selected_db_marker']=='new-cwd-decoy',r
 else:
  assert r['phase1_rc']==r['decision_rc']==r['install_rc']==0 and r['generated_db_arg'].startswith('/') and r['generated_identity_arg']==r['input_identity'] and r['selected_db_marker']=='runtime-real',r
print('ALL_EXPECTATIONS_MATCHED',flush=True)
```
