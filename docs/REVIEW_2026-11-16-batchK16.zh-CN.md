# 批次 K 修复轮十五独立复核（第 16 轮，2026-11-16）

历史归属补注（2026-11-16）：本报告及出具时的本地同步随后由执行方在 `f2574cb` 入库，`7a93314` 仅回填该修复提交号。本页保留当时的历史裁决；新候选见 [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md)。

复核范围：`dev/batch-k-20261114` 的 `f425256..0602f92`；实现与文档提交为 `0d58853`，`0602f92` 只在交接回填一行提交号。固定 tip 为 `0602f9270ae38fd9a6070c762b3c5a9214d4401a`。结论为 **REJECTED**：原 environ 验证读失败和普通相对数据库路径反例均已翻转，但读取失败的非空输出仍被采纳，变量提取错误新增了回落 HOME 的放行路径，合法方括号数据库文件名仍可逃过绝对化。

逐项裁决只在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。命令、运行编号、计数、仓外材料与未执行项集中在 [K 交接 §37](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#37-修复轮十五独立复核证据2026-11-16)。本轮只新增评审报告并同步本地文档，不修改被审实现，不创建提交，不合并、推送或删除分支。下列源码行号均指固定 tip。

已使用当前 git 凭据直接读取 GitHub run/jobs API；首次查询时固定 tip 已完成，主复核随后独立再次查询并核对完整 SHA、分支、全部常规 job 及步骤。CI 成功，手动 release 深测跳过不算执行；CI 不覆盖以下独立行为反例。报告日期按本轮指定的 2026-11-16，API 的 UTC 时间按实际响应保留在交接证据表。

## 1. K10-N1 / P1：单次快照没有核验读取成功，部分输出仍可触发错源迁移

`scripts/deploy_linux.sh:978` 生成的 phase1 只有 `set -eu`，没有 `pipefail`。`:1269` 的管道未显式检查左侧 cat 的状态：

```bash
sudo -n cat /proc/$reg_pid/environ 2>/dev/null | tee "$reg_env_raw" >/dev/null
```

`:1270` 起只检查快照是否非空。后续总量、值与字节计数确实均来自这一文件，但文件非空不能证明读取成功或完整。

输入为同一进程的真实 NUL 记录模型：`HOME=/home/otheruser`、`USERPROFILE=/home/runtime-real`、`USER=otheruser`。unit 使用 EnvironmentFile，没有显式 `--id-dir`。正确来源 `/home/runtime-real/.ak-sending` 内放合成 `runtime-real-*`；HOME 对应目录放 `fallback-decoy-*`。

仅让 cat 输出原始 environ 的第一个完整 HOME 记录后返回 74，保留 `REVIEW_CAT_PARTIAL_ERROR=74` 标记。tee 成功使整个管道返回零；非空快照缺少本应优先的 USERPROFILE。实际 phase1、decision、install 均返回零，确认 HOME 路径，停旧服务后真实复制 `fallback-decoy-registry.key` 与 `fallback-decoy-registry.db`。当前只有一次 environ 读取；同夹具在 `f425256` 也错源，是既有余项。普通控制复制正确来源；cat 完整输出后返回非零的控制也继续成功，进一步确认失败状态未被采纳。

新增 DB cwd 分支 `:1468` 存在同一管道问题。readlink 模型输出完整 `/home/runtime-old\n`（最后为工具的 LF 终止符），记录失败标记并返回 1，实际 phase1、decision、install 仍为零。此案输出的路径本身正确，只证明读取失败被忽略，不把它单独描述成错库。无输出的 cwd 失败与真实尾 LF 路径仍在停服前拒绝。

已知存在变量的字节证据提取失败、无 stdout 时，当前 `:1307` 正确拒绝；tee 部分写入后返回非零也由 `set -e` 终止。问题集中在管道左侧失败未进入判断，不能将上述有效控制一并判坏。

**修复要求：** 捕获时同时核验读取命令和快照写入命令的成功状态；任何非零状态即使附带完整或部分 stdout，也应在停服前拒绝。所有 cwd 捕获分支应采用相同纪律，不能只增加非空和长度检查。保留单次读取、原尾 LF、已知存在变量证据不可用及普通控制。

## 2. K10-N1 / P1：变量提取错误被当成 absent，是本次新增回归

`scripts/deploy_linux.sh:1283–1284` 将正常无匹配与执行错误统一吞掉：

```bash
ev_up_rec=$(grep -za -m1 '^USERPROFILE=' "$reg_env_raw" || true)
ev_home_rec=$(grep -za -m1 '^HOME=' "$reg_env_raw" || true)
```

随后新增的 `:1294` 使用 `[ -n "$re_val" ] || continue`，把空解析结果直接视为变量不存在。`:1322–1325` 因而允许回落 HOME。

仍使用 §1 的完整 environ，cat 成功且只读一次；仅让 USERPROFILE 的 `grep -za -m1` 返回 2、无 stdout，并记录 `REVIEW_GREP_EXTRACT_ERROR=2`。其他提取、字节检查和文件操作正常。当前完整链全部返回零，实际复制 HOME 诱饵目录的 key/db。

同一故障在 `f425256` 的 phase1 返回 3、没有停服或目标 key：旧版没有新加入的空解析值跳过，仍比较变量记录的字节数，发现解析值与实际 USERPROFILE 记录不一致。故这一放行是 `0d58853` 新引入的回归，不能与 §1 一起归为既有缺口。最初复核 runner 曾把此基线也预期为放行，实际输出推翻该假设；修正的是测试预期，并保留首次输出和补跑证据，没有修改产品代码。

**修复要求：** 提取时明确区分匹配成功、正常无匹配与工具错误。只有确证正常无匹配才可作为 absent 输入默认链；工具错误必须拒绝。不能由空字符串推断变量不存在，也不能以单次 `/proc` 读取替代后续提取成功的检查。

## 3. K15-N1 / P1：方括号文件名使改写静默失效，生成 unit 仍指向另一数据库

当前 unit 和逐参数 NUL cmdline 完全一致：

```ini
[Service]
User=otheruser
WorkingDirectory=/home/runtime-old
ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200 --id-dir /home/otheruser/.ak-sending --db-path home/otheruser/.ak-sending/db[1].sqlite
```

运行 cwd 下的原 SQLite 对象是 `/home/runtime-old/home/otheruser/.ak-sending/db[1].sqlite`，marker 为 `runtime-real`；新默认 cwd `/` 下的同名相对对象是 `/home/otheruser/.ak-sending/db[1].sqlite`，marker 为 `new-cwd-decoy`。方括号是合法普通文件名字节，不涉及命令执行或特殊 systemd 展开。

`scripts/deploy_linux.sh:2468–2469` 把原路径放进 Bash 模式替换：

```bash
REGISTRY_EXEC=${REGISTRY_EXEC//--db-path $rel_db/--db-path $abs_db}
REGISTRY_EXEC=${REGISTRY_EXEC//--db-path=$rel_db/--db-path=$abs_db}
```

`[1]` 在模式中匹配单个 `1`，不匹配输入中的字面 `[1]`。两条替换均没有发生，`:2470` 仍记录“rewritten”，后续也没有断言生成参数已经绝对化。`:2412` 的白名单只检查 DATA_SOURCE_DIR 和当时的 REG_RW，没有覆盖这个数据库参数。

当前真实 phase1→decision→compose→write_phase456→install 全部成功；cwd 只读一次，最终生成：

```ini
ExecStart=<new-binary> registry-server --port 17200 --id-dir /home/otheruser/.ak-sending --db-path home/otheruser/.ak-sending/db[1].sqlite
ReadWritePaths=/home/otheruser/.ak-sending /home/runtime-old/home/otheruser/.ak-sending
```

没有 `WorkingDirectory=`。从实际生成参数按系统服务默认 cwd 解析并读取真实 SQLite，得到 `new-cwd-decoy`。诱饵的父目录就在已有 ReadWritePaths 中，不能说 ProtectSystem 必然拦截。CLI 保留显式 PathBuf 并交给 SQLite 的代码为 `crates/ak-cli/src/main.rs:3792`、`crates/ak-signal/src/server_v2/store.rs:397`。

`f425256` 相同输入也保留相对形式；当前仅把参数改成原对象绝对路径的控制保持 `runtime-real`。这证明原缺口在该输入上仍未修复，并不把它归为本轮新引入的旧缺陷。原报告的普通 `registry.db` 相对路径已经正确翻转，生成的 DB 参数及新增可写目录均来自运行 cwd，原基线仍指向诱饵。

**修复要求：** 按字面参数改写并验证最终对象；无法忠实处理时在停服前拒绝。绝对化后应检查生成 unit 的所有身份/数据库参数及可写目录确实指向已解析对象，不能以替换语句执行过或日志文字代替结果校验。保留方括号、普通路径、等号参数、混合相对/绝对身份和 DB 参数的完整链回归。

## 4. 保留的修复与验证边界

主复核按 K15 原报告重建真实尾 LF、仅第四次 environ 读取失败且随后恢复，以及从第四次起持续失败的输入。当前均拒绝且只读一次，旧 `f425256` 的失败标记实际触发并全链错源。专项还直接用现有 `ENV_READ_FAIL_FROM=4` 重放原输入；没有以首次读取失败替代原反例。普通环境、稳定特殊字节和历史绿场景由本轮门槛及专项控制分别保留。

显式相对 id＋相对 DB、相对 id＋绝对 DB、`--id-dir=... --db-path=...` 已验证正确对象及单次 cwd 读取。环境推导的相对或空身份基值配相对 DB 时，当前因未报告已捕获的 runtime-cwd 而在 decision 保守拒绝；此项不是本轮拒绝依据。绝对 DB 在身份目录外时仍未增加对应 ReadWritePaths，这是原行为和验证边界，没有将未执行的真实权限结果判为本轮新增阻断。

生产生成器、决策和安装 Bash 片段按 `git show` 的对应版本真实执行；决策值在同一 Bash 内流入生成器，没有手写替换 CHOSEN。cp/cmp 与 SQLite 文件读取真实执行；proc、sudo、systemctl、权限和故障为隔离模型。没有声称执行 Linux 原生部署、生产服务或物理双机验证。本轮没有另行启动原生 registry CLI；上轮原生证据仍按其历史范围保留。

## 5. 可复现步骤

在固定候选仓库根目录执行。将下列 Python 代码分别保存到两个新建的 TEMP 子目录，文件名为 `review_environ.py`、`review_bracket.py`，再运行所示命令。runner 以自身目录保存证据，所有输入均为合成数据；不要存入仓库。退出零表示反例及控制满足断言，不表示产品通过。当前脚本源码固定从 git 版本提取，故本地文档同步不改变被审对象。

### 5.1 快照读取状态、提取错误与拒绝控制

```powershell
python <TEMP目录>/review_environ.py ordinary capture-partial-error capture-partial-error-baseline extract-profile-error extract-profile-error-baseline present-record-evidence-error tee-capture-error
```

```python
import importlib.util, json, re, shutil, subprocess, sys, tempfile
from pathlib import Path

ROOT = Path.cwd()
OUT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('deploy_tests', ROOT / 'scripts/tests/test_deploy_linux_install.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

CASES = [
    ('ordinary', '0602f92', 'none', False),
    ('capture-partial-error', '0602f92', 'cat-prefix-error', False),
    ('capture-partial-error-baseline', 'f425256', 'cat-prefix-error', False),
    ('capture-full-error', '0602f92', 'cat-full-error', False),
    ('extract-profile-error', '0602f92', 'extract-error', False),
    ('extract-profile-error-baseline', 'f425256', 'extract-error', False),
    ('present-record-evidence-error', '0602f92', 'evidence-error', False),
    ('tee-capture-error', '0602f92', 'tee-error', False),
    ('prior-fourth-read-error-current', '0602f92', 'fourth-error', True),
    ('prior-fourth-read-error-baseline', 'f425256', 'fourth-error', True),
    ('stable-tail-lf', '0602f92', 'none', True),
]
if len(sys.argv) > 1:
    CASES = [x for x in CASES if x[0] in sys.argv[1:]]

results=json.loads((OUT/"results.json").read_text(encoding="utf-8")) if (OUT/"results.json").exists() else []
for label, rev, mode, tail_lf in CASES:
    h=m.DeployInstallTests()
    h.setUp()
    try:
        h.text=subprocess.check_output(['git','show',rev+':scripts/deploy_linux.sh']).decode('utf-8')
        profile=b'/home/runtime-real' + (b'\n' if tail_lf else b'')
        env=b'HOME=/home/otheruser\0USERPROFILE='+profile+b'\0USER=otheruser\0'
        h.make_private_discovery_model(main_exec=h.K9_BASE,cmdline_extra='--port 17200',
            unit_service_lines=['User=otheruser','EnvironmentFile=/etc/default/oldregistry'],environ=env)
        envfile=h.probe/'etc/default/oldregistry'
        envfile.parent.mkdir(parents=True,exist_ok=True)
        envfile.write_bytes(b'HOME=/home/otheruser\nUSERPROFILE="'+profile+b'"\n')
        for d,marker in [('home/runtime-real/.ak-sending','runtime-real'),('home/otheruser/.ak-sending','fallback-decoy')]:
            p=h.probe/d
            p.mkdir(parents=True,exist_ok=True)
            for name in ('registry.key','registry.db'):
                (p/name).write_text(marker+'-'+name,encoding='utf-8')
        h.driver_env['REVIEW_MODE']=mode
        cat=h.probe/'bin/cat'
        s=cat.read_text(encoding='utf-8')
        inj=r'''if [ "$*" = /proc/4242/environ ]; then
  if [ "${REVIEW_MODE:-none}" = cat-prefix-error ]; then
    printf 'REVIEW_CAT_PARTIAL_ERROR=74\n' >> "$PROBE_DIR/stubs.log"
    printf 'HOME=/home/otheruser\0'
    exit 74
  elif [ "${REVIEW_MODE:-none}" = cat-full-error ]; then
    printf 'REVIEW_CAT_FULL_ERROR=74\n' >> "$PROBE_DIR/stubs.log"
    @REAL_CAT@ "$PROBE_DIR/proc/4242/environ"
    exit 74
  elif [ "${REVIEW_MODE:-none}" = fourth-error ] && [ "$n" = 4 ]; then
    printf 'REVIEW_ENV_FOURTH_ERROR=74\n' >> "$PROBE_DIR/stubs.log"
    exit 74
  fi
fi
'''.replace('@REAL_CAT@',m._posix_tool('cat'))
        assert s.count('mapped=()\n') == 1
        cat.write_text(s.replace('mapped=()\n',inj+'mapped=()\n'),encoding='utf-8',newline='\n')
        grep=h.probe/'bin/grep'
        grep.write_text(r'''#!/usr/bin/env bash
set -u
if [ "${1:-}" = -za ] && [ "${3:-}" = '^USERPROFILE=' ] && [ "${REVIEW_MODE:-none}" = extract-error ]; then
  printf 'REVIEW_GREP_EXTRACT_ERROR=2\n' >> "$PROBE_DIR/stubs.log"
  echo 'review model: USERPROFILE extraction read error' >&2
  exit 2
fi
if [ "${1:-}" = -zao ] && [ "${3:-}" = '^USERPROFILE=.*' ] && [ "${REVIEW_MODE:-none}" = evidence-error ]; then
  printf 'REVIEW_GREP_EVIDENCE_ERROR=2\n' >> "$PROBE_DIR/stubs.log"
  echo 'review model: USERPROFILE evidence read error' >&2
  exit 2
fi
exec @REAL_GREP@ "$@"
'''.replace('@REAL_GREP@',m._posix_tool('grep')),encoding='utf-8',newline='\n')
        grep.chmod(0o755)
        tee=h.probe/'bin/tee'
        s=tee.read_text(encoding='utf-8')
        inj=r'''if [ "${REVIEW_MODE:-none}" = tee-error ] && [[ "$dest" == */ak-env-bytes.* ]]; then
  cat >/dev/null
  printf 'HOME=/home/otheruser\0' > "$out"
  printf 'REVIEW_TEE_PARTIAL_ERROR=74\n' >> "$PROBE_DIR/stubs.log"
  exit 74
fi
'''
        assert s.count('cat > "$out"\n')==1
        tee.write_text(s.replace('cat > "$out"\n',inj+'cat > "$out"\n'),encoding='utf-8',newline='\n')
        body=r'''
WORK="$PROBE_DIR"
KEEP_LEGACY_SIGNAL=0
CHECK_ONLY=0
write_phase1 "$PROBE_DIR/phase1.sh" || exit 90
bash "$PROBE_DIR/phase1.sh" > "$PROBE_DIR/phase1.log" 2>&1
phase_rc=$?
printf 'PHASE1_RC=%s\n' "$phase_rc"
[ "$phase_rc" = 0 ] || exit 0
review_decision() {
''' + h.decision_block(h.text,rev) + h.decisions_block(h.text,rev) + r'''
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
bash "$PROBE_DIR/phase456.sh" > "$PROBE_DIR/phase456.log" 2>&1
printf 'INSTALL_RC=%s\n' "$?"
'''
        driver_rc,stdout=h.run_driver(h.definitions()+m.BOX_MODELS,body,name='driver.sh',timeout=180)
        (h.probe/'driver.out').write_text(stdout,encoding='utf-8')
        phase=(h.probe/'phase1.log').read_text(encoding='utf-8')
        log=h.stubs_log()
        phase_rc=int(re.search(r'^PHASE1_RC=(\d+)$',stdout,re.M).group(1))
        row={'label':label,'revision':rev,'mode':mode,'phase1_rc':phase_rc,'driver_rc':driver_rc,
            'markers':re.findall(r'^(?:REVIEW_|ENV_).*$',log,re.M),
            'reported_id':re.findall(r'^  runtime-id-dir: (.*)$',phase,re.M),
            'stopped':bool(re.search(r'systemctl stop ',log)),
            'phase_errors':re.findall(r'^ERROR:.*$',phase,re.M)}
        assert driver_rc == 0,stdout
        if phase_rc==0:
            assert 'DECISION_RC=0' in stdout,stdout
            assert 'INSTALL_RC=0' in stdout,stdout
            copied={name:(h.probe/'opt-ak/data'/name).read_text(encoding='utf-8') for name in ('registry.key','registry.db')}
            row.update(decision_rc=0,install_rc=0,chosen=dict(re.findall(r'^CHOSEN_(\w+)=(.*)$',stdout,re.M)),copied=copied)
        else:
            assert 'DECISION_RC=' not in stdout,stdout
            assert not row['stopped'],log
            assert not (h.probe/'opt-ak/data/registry.key').exists()
        if mode == 'extract-error' and rev == 'f425256':
            assert phase_rc==3,row
            assert 'lost trailing bytes' in phase,row
        elif mode in ('cat-prefix-error','extract-error'):
            assert phase_rc==0,row
            assert row['reported_id']==['/home/otheruser/.ak-sending'],row
            assert row['copied']=={n:'fallback-decoy-'+n for n in ('registry.key','registry.db')},row
        elif mode=='cat-full-error':
            assert phase_rc==0,row
            assert row['copied']=={n:'runtime-real-'+n for n in ('registry.key','registry.db')},row
        elif mode=='evidence-error' or (tail_lf and (rev=='0602f92' or mode=='none')):
            assert phase_rc==3,row
        elif mode=='tee-error':
            assert phase_rc==74,row
        elif tail_lf and rev=='f425256' and mode=='fourth-error':
            assert phase_rc==0,row
            assert row['reported_id']==['/home/runtime-real/.ak-sending'],row
        else:
            assert phase_rc==0,row
            assert row['copied']=={n:'runtime-real-'+n for n in ('registry.key','registry.db')},row
        if rev=='0602f92':
            assert row['markers'].count('ENV_READ_1')==1,row
            assert 'ENV_READ_2' not in row['markers'],row
        dest=OUT/label
        shutil.copytree(h.probe,dest,dirs_exist_ok=True)
        (dest/'result.json').write_text(json.dumps(row,ensure_ascii=False,indent=2),encoding='utf-8')
        results=[x for x in results if x['label']!=label]
        results.append(row)
        (OUT/'results.json').write_text(json.dumps(results,ensure_ascii=False,indent=2),encoding='utf-8')
        print(json.dumps(row,ensure_ascii=False),flush=True)
    finally:
        h.doCleanups()
print('EVIDENCE_DIR='+str(OUT))
```

### 5.2 相对数据库方括号路径、历史基线与绝对控制

```powershell
python <另一个新TEMP目录>/review_bracket.py
```

```python
import importlib.util, json, re, shutil, sqlite3, subprocess, sys
from pathlib import Path
root = Path.cwd()
spec=importlib.util.spec_from_file_location('deploy_tests', root/'scripts/tests/test_deploy_linux_install.py')
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
outdir=Path(__file__).parent
cases=[
 ('relative-bracket-db-tip','0602f92','/home/otheruser/.ak-sending','home/otheruser/.ak-sending/db[1].sqlite','/home/otheruser','normal'),
 ('relative-bracket-db-baseline','f425256','/home/otheruser/.ak-sending','home/otheruser/.ak-sending/db[1].sqlite','/home/otheruser','normal'),
 ('absolute-bracket-db-control','0602f92','/home/otheruser/.ak-sending','/home/runtime-old/home/otheruser/.ak-sending/db[1].sqlite','/home/otheruser','normal'),
]

results=[]
for label,rev,identity,dbarg,profile,mode in cases:
 h=m.DeployInstallTests();h.setUp()
 try:
  h.text=subprocess.check_output(['git','show',rev+':scripts/deploy_linux.sh']).decode('utf8')
  sep='=' if mode=='equals' else ' '
  args='--port 17200'+(' --id-dir'+sep+identity if identity else '')+' --db-path'+sep+dbarg
  h.make_private_discovery_model(main_exec='/opt/ak-sending/ak-cli registry-server '+args,cmdline_extra=args,unit_service_lines=['User=otheruser','WorkingDirectory=/home/runtime-old','EnvironmentFile=/etc/default/oldregistry'],environ=b'HOME=/home/otheruser\0USERPROFILE='+profile.encode()+b'\0USER=otheruser\0')
  envfile=h.probe/'etc/default/oldregistry';envfile.parent.mkdir(parents=True,exist_ok=True);envfile.write_text('HOME=/home/otheruser\nUSERPROFILE='+profile+'\n',encoding='utf8')
  liveid=identity if identity else ((profile.rstrip('/')+'/.ak-sending') if profile else '.ak-sending')
  if not liveid.startswith('/'):liveid='/home/runtime-old/'+liveid
  live=h.probe/liveid.lstrip('/');live.mkdir(parents=True,exist_ok=True)
  (live/'registry.key').write_text('synthetic-runtime-key',encoding='utf8')
  live_db='/home/runtime-old/'+dbarg if not dbarg.startswith('/') else dbarg
  alt_db='/'+dbarg if not dbarg.startswith('/') else None
  for obj,marker in [(live_db,'runtime-real'),(alt_db,'new-cwd-decoy')]:
   if obj is None:continue
   path=h.probe/obj.lstrip('/');path.parent.mkdir(parents=True,exist_ok=True)
   if path.exists():path.unlink()
   con=sqlite3.connect(path);con.execute('CREATE TABLE review_marker(value TEXT)');con.execute('INSERT INTO review_marker VALUES (?)',(marker,));con.commit();con.close()
  h.driver_env['PROC_CWD_MAP']='/proc/4242/cwd='+('unreadable' if mode=='unreadable' else '/home/runtime-old')
  h.driver_env['CWD_FAIL_ON_SECOND']='1'
  if mode=='tail-lf':
   raw=h.probe/'cwd-raw';raw.write_bytes(b'/home/runtime-old\n');h.driver_env['PROC_CWD_RAW_FILE']=raw.as_posix()
  if mode=='read-failure':
   st=h.probe/'bin/readlink';txt=st.read_text(encoding='utf8')
   txt=txt.replace('if [ -n "${PROC_EXE_MAP:-}" ]; then', '''if [ "$target" = /proc/4242/cwd ]; then
  printf '/home/runtime-old\\n'
  printf 'CWD_NONZERO_WITH_OUTPUT\\n' >> "$PROBE_DIR/stubs.log"
  exit 1
fi
if [ -n "${PROC_EXE_MAP:-}" ]; then''',1)
   st.write_text(txt,encoding='utf8',newline='\n')
  body=r'''
WORK="$PROBE_DIR"
KEEP_LEGACY_SIGNAL=0
CHECK_ONLY=0
write_phase1 "$PROBE_DIR/phase1.sh" || exit 90
bash "$PROBE_DIR/phase1.sh" > "$PROBE_DIR/phase1.log" 2>&1
phase_rc=$?
printf 'PHASE1_RC=%s\n' "$phase_rc"
[ "$phase_rc" = 0 ] || exit 0
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
bash "$PROBE_DIR/phase456.sh" > "$PROBE_DIR/phase456.log" 2>&1
printf 'INSTALL_RC=%s\n' "$?"
'''
  driver_rc,stdout=h.run_driver(h.definitions()+m.BOX_MODELS,body,name='driver.sh',timeout=120)
  (h.probe/'driver.out').write_text(stdout,encoding='utf8')
  p1=(h.probe/'phase1.log').read_text(encoding='utf8');log=h.stubs_log()
  rec={'case':label,'revision':rev,'driver_rc':driver_rc,'phase1_rc':int(re.search('PHASE1_RC=(\\d+)',stdout)[1]),'runtime_fields':[x for x in p1.splitlines() if re.match(r'  runtime-',x)],'cwd_reads':len(re.findall(r'^readlink /proc/4242/cwd$',log,re.M)),'cwd_failure_marker':'CWD_NONZERO_WITH_OUTPUT' in log,'second_read_failed':'review-cwd-second-read-failed' in log}
  dec=re.search('DECISION_RC=(\\d+)',stdout);ins=re.search('INSTALL_RC=(\\d+)',stdout)
  if dec:rec['decision_rc']=int(dec[1])
  if ins:rec['install_rc']=int(ins[1])
  rec['chosen']=dict(re.findall(r'^CHOSEN_(\w+)=(.*)$',stdout,re.M))
  unit=h.probe/'etc/systemd/system/ak-registry-server.service'
  if unit.exists():
   u=unit.read_text(encoding='utf8');rec['unit_fields']=[x for x in u.splitlines() if x.startswith(('ExecStart=','ReadWritePaths=','User=','WorkingDirectory='))]
   db=re.search(r'--db-path[= ]([^ ]+)',u)[1].splitlines()[0]
   obj=h.probe/db.lstrip('/');rec['generated_db_arg']=db
   con=sqlite3.connect(obj);rec['selected_db_marker']=con.execute('SELECT value FROM review_marker').fetchone()[0];con.close()
  dst=outdir/label;shutil.copytree(h.probe,dst)
  results.append(rec);print(json.dumps(rec),flush=True)
 except Exception as e:
  (outdir/'results-bracket.partial.json').write_text(json.dumps(results,indent=2),encoding='utf8');print('FAILED '+label+': '+repr(e),flush=True);raise
 finally:h.doCleanups()
(outdir/'results-bracket.json').write_text(json.dumps(results,indent=2),encoding='utf8')
assert results[0]['phase1_rc']==results[0]['decision_rc']==results[0]['install_rc']==0
assert results[0]['generated_db_arg']=='home/otheruser/.ak-sending/db[1].sqlite', results[0]
assert results[0]['selected_db_marker']=='new-cwd-decoy', results[0]
assert results[0]['cwd_reads']==1 and not results[0]['second_read_failed'], results[0]
assert results[1]['generated_db_arg']==results[0]['generated_db_arg'] and results[1]['selected_db_marker']=='new-cwd-decoy', results[1]
assert results[2]['generated_db_arg'].startswith('/') and results[2]['selected_db_marker']=='runtime-real', results[2]
print('PASS: bracket-relative counterexample and baseline/absolute controls reproduced',flush=True)
print('EVIDENCE_DIR='+str(outdir),flush=True)
```

该路径 runner 保留了 readlink 故障分支。要复现 §1 的 cwd 旁证，可在 `cases` 末尾追加 `('relative-db-cwd-output-then-fail','0602f92','/home/otheruser/.ak-sending','registry.db','/home/otheruser','read-failure')`，并断言新增结果的 `cwd_failure_marker` 为真、`cwd_reads == 1`、`phase1_rc == decision_rc == install_rc == 0`；将 mode 改为 `unreadable` 的控制应在 phase1 返回 3。主复核已分别执行这两个输入，原始 runner 与证据位置见交接证据表。
