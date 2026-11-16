# 批次 K 修复轮二十独立复核（第 21 轮，2026-11-16）

复核范围：`dev/batch-k-20261114` 的 `110bb34..714099a`；实现与文档提交 `f4b58a6`，`714099a` 只回填交接的一行哈希。固定 tip 为 `714099a974b33e7f4c0d91076c03af1aed15070a`。结论为 **REJECTED**：K20 的 glob 展开与空值/后继长选项反例均已修复，但身份参数的子串误解析以及 `/home` 目录的空白分割仍可选错迁移来源。两个新发现均由直接基线自身完整链证实为既有缺口，不归因于本轮新增回归。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。实际命令、运行编号、计数、仓外材料及未执行范围统一记在 [K 交接 §47](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#47-修复轮二十独立复核证据2026-11-16)。固定 tip CI 已通过 GitHub API 等到 completed/success；CI 与现有回归不能覆盖下述独立反例。本轮只同步本地审核文档，不修改被审实现，不创建提交、合并、推送或删除分支。以下行号均指固定 tip。

> 归属补注（K22，2026-11-16）：本文及交接 §47、STATUS、审核状态表已随 `6964239` 纳入仓库，`a369e1c` 仅回填执行方 §48 的实现哈希；上文“本轮只同步本地”描述 K21 当时动作。后续复核见 [K22](REVIEW_2026-11-16-batchK22.zh-CN.md)。

## 1. K20 修复的实际效果

`scripts/deploy_linux.sh` 的 phase1 `bound_db_arg`、`db_binding_invalid`、`/home` 枚举及 decision 的 DB 提取、改写、校验遍历均已限定在 `set -f` / `set +f` 内。用 K20 报告中的原始完整链夹具、分离的 phase1 cwd、真实方括号 SQLite 和普通名称诱饵重放，当前空格/等号拼写在本机 cwd 有/无匹配文件时都拒绝 **未展开的** `db[1].sqlite`，没有停服或生成 unit；普通文件名完整链正确。`110bb34` 自身 phase1→decision→install 则仍停服，并按生成 unit 中的参数读取到诱饵 SQLite。K20-N1 的精确修复有效。

`db_binding_invalid` 在 `:1205–1206` 先检查 configured 和 running 两侧，然后才进入比较。空等号后缀和空格形式的后继长选项均在 phase1 返回 3、停服前拒绝；空值诊断包含真实 CLI 的 `a value is required`。悬空、重复及混合拼写的两侧 helper 检查也拒绝；合法 `--db-path=--db-path` 完整链保持，并从生成参数实际打开 SQLite 确认对象。直接基线自身完整链仍接受 K20 的非法形态。K20-N2 的精确反例已翻转。

旧 K19-N2 对照现在经 `_k20_decision_of(..., baseline_rev=...)` 使用基线自身 `phase_defs` 与 decision，避免把当前预检的拒绝结果喂给旧实现；该修正符合历史对照目的。全文件 `for…in` 走查中，其余执行循环使用引用值、固定字面量或生成器写入的服务/状态列表；后者包含发现得到的服务名，不应扩大描述成所有来源都是真正固定常量。

## 2. K21-N1 / P1：合法身份目录中的选项字样被误解析，停服后复制诱饵并生成无效 unit

配置与运行 argv 完全相同，路径只含脚本允许的字符：

```text
registry-server --id-dir /home/otheruser/--id-dir --port 17200
```

夹具将运行 cwd 绑定为 `/home/runtime-old`。真实目录 `/home/otheruser/--id-dir` 放置 `runtime-real-key`、`runtime-real-db` 标记；独立诱饵目录 `/home/runtime-old/--port` 放置 `substring-decoy-key`、`substring-decoy-db`。这些均是映射到 TEMP 的合成路径与内容。

`scripts/deploy_linux.sh:1223–1224` 的两侧身份比较、`:1427` 的运行身份提取、`:2665` 的 ReadWritePaths 提取及 `:2712` 的改写前提取仍用贪婪 `s/.*--id-dir[= ].../`。它们把路径末尾的字面 `--id-dir` 当作另一个选项起点，将后继 `--port` 误认成身份值。两侧同时误解析而通过比较；phase1 把误取的相对值绑定到运行 cwd，报告：

```text
runtime-id-dir: /home/runtime-old/--port
```

随后 `:2716–2717` 的字符串替换又命中路径内部的 `--id-dir --port`，破坏了真正的 port 选项。实际完整链中，phase1、decision、install 均返回 0，旧服务 `exists:enabled:active` 被捕获并执行 stop；目标 `opt-ak/data/registry.key` 和 `registry.db` 的实际内容来自诱饵。生成 unit 的关键字段是：

```text
ExecStart=<新安装的ak-cli> registry-server --id-dir /home/otheruser/--id-dir /home/runtime-old/--port 17200
ReadWritePaths=/home/runtime-old/--port
```

主复核独立重放确认，空格和等号身份拼写均可触发；普通 `/home/otheruser/identity` 控制正确迁移真实标记且保留参数。当前实际构建、版本标记 git714099a 的 CLI 接受原始路径形态，在预设无效长度的合成 key 处返回 1；对生成 argv 则返回 2，报 `unexpected argument '/home/runtime-old/--port' found`。非法生成参数探针额外附加永久无效 port，原始合法形态探针在身份加载阶段预设失败，均不会创建服务器。

直接基线 `110bb34` 自身生成器和完整链同样复制诱饵、停服并生成损坏参数。`git blame` 将身份比较定位到 `7e8bfb2`、运行身份提取定位到 `e03a44f`、相对身份改写定位到 `d38cb01`。这是本轮新发现的既有阻断，不能称为 `f4b58a6` 新增回归；K19/K20 对 DB 选项的修复没有覆盖身份选项。

**修复要求：** 身份选项的比较、运行路径推导、ReadWritePaths 推导、改写及结果验证使用同一套完整 token 绑定语义；值内的选项字样不得再次识别为选项。保留真实字面路径或在停服前明确拒绝，不能仅验证生成字符串含某个子串。回归应检查最终 argv、实际复制的 key/db 字节、旧 active 状态与停服行为，并保留两种拼写及普通目录控制。

## 3. K21-N2 / P2：`/home` 枚举仍按空白分割完整路径，前缀目录可被选为迁移源

`scripts/deploy_linux.sh:1787–1795` 先用 `ls -1` 和 `sed` 构造完整候选，然后在未引用的 `$home_ak_dirs` 上执行 word splitting。`set -f` 消除了 pathname expansion，但不会保留带空格的目录记录。无 live occupant 的合成主机包含：

```text
/home/other user/.ak-sending/registry.key = synthetic-existing-key
/home/other user/.ak-sending/registry.db  = synthetic-existing-db
/home/other/registry.key                 = synthetic-split-decoy-key
/home/other/registry.db                  = synthetic-split-decoy-db
```

实际 GNU `ls -1` 输出 `other` 和 `other user`。正常枚举应检查 `/home/other/.ak-sending`（此目录不存在）及带空格的完整目录；实际循环把后者拆成 `/home/other` 与相对的 `user/.ak-sending`，于是产生：

```text
registry-data: /home/other (key:yes db:yes)
CHOSEN_DATA_SOURCE=/home/other
```

`:2530–2537` 将这个伪候选视为唯一来源。主复核独立执行当前与 `110bb34` 自身完整链，phase1、decision、install 均返回 0，目标 key 和 db 的实际字节均来自 split-decoy，并调用模型 `systemctl enable --now`。真实来源文件保持原样。本案没有旧 active occupant，也没有 stop；不声称已进行原生 systemd 启动或生成了新身份。

补充对照证明这还绕过既有的多候选拒绝：真实存在 `other user/.ak-sending` 和 `otheruser/.ak-sending` 时只报告后者并迁移；把目录改成两个普通名称时，真实 decision 会报告 `multiple identity directories` 并在 install/enable 前拒绝。单个普通名称则正确迁移。若完整的带空格路径到达 `:2676–2681` 的现有白名单，本应停服前拒绝；分割反而把不支持的路径变成可接受的另一来源。

本轮 runner 只在 TEMP 模型中把 `ls /home` 改为真正列出合成目录的 GNU `ls`；仓库原 `LS_HOME_ENTRIES` stub 自己也按空白分词，不能忠实提供该输入。其余 proc、systemctl、权限模型和从固定提交提取的实现均未变。`git blame` 将未引用目录循环定位到 `f7dece2`；本轮新增 noglob 没有引入此空白问题。

**修复要求：** 保留目录记录的完整边界，不把目录名重新当作 Shell 单词列表；不能忠实表达的名字应在变更前明确拒绝。候选数量必须基于实际目录对象，不能漏掉不支持的候选后继续选择其他来源。回归需让枚举模型真实提供带空白名字，并检查伪前缀诱饵、真实多候选、无诱饵及普通控制下的最终文件内容和拒绝时点。

## 4. 证据与声明边界

固定 tip CI 已完成且通过，常规本机门槛和完整 Python 回归的实际结果只见交接证据表。CI 的手动 release 深测跳过不算通过；K20 已记录的本机 release chunk sweep 超时未在本轮重新运行或解决。Rust、依赖及 workflow 在本轮范围内没有变化，不能把历史超时归因于本轮 Bash 修复，也不能将本轮常规通过扩大为 release 深测通过。

另有非独立阻断的描述边界：当前 `--*` 检查并非完整镜像 CLI 的所有选项语法。真实 CLI 也会拒绝空格形式 `--db-path -h`，当前模型会将它改写为绝对文件名后接受；本轮没有证明该补充形态在改写后读错对象或仍无法启动，因此没有单列实害发现。K20-N2 的通过判断限定在已重放的形态。

所有新反例使用本地合成文件和仓库已有离线模型；Bash、生成脚本、复制及 CLI 解析真实执行，proc/systemctl/权限仍为模型。没有生产访问、SSH、物理双机或抓包，没有 relay、真实密钥或用户文件入库。当前 CI 的 Linux/Android 构建和网络命名空间模拟与本机模型分别记录，不能合称原生部署验收。

## 5. 可重放代码

将以下代码分别保存到仓外新的 TEMP 目录，从仓库根运行。固定提交需存在于本地历史。`ALL_EXPECTATIONS_MATCHED` 表示缺陷与控制符合断言，不表示产品通过。第一份需要 `cargo build --locked --workspace` 生成当前 Windows debug CLI；其他平台应按实际二进制名称调整。报告中的代码与主复核实际执行的 runner 逐字一致，材料仅含合成路径和标记。

### 5.1 身份参数的当前、基线与普通控制

```powershell
python <TEMP目录>/review_identity.py --output <新的证据目录>
```

```python
import importlib.util,json,re,shlex,shutil,sqlite3,subprocess,sys,time
from pathlib import Path
ROOT=Path.cwd(); selection=sys.argv[1:]; OUT=Path(__file__).parent/'identity-evidence'
if '--output' in selection:
 i=selection.index('--output');OUT=Path(selection[i+1]);del selection[i:i+2]
OUT.mkdir(parents=True,exist_ok=True)
spec=importlib.util.spec_from_file_location('deploy_tests',ROOT/'scripts/tests/test_deploy_linux_install.py')
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
identity='/home/otheruser/.ak-sending'
base='--id-dir '+identity+' --port 17200'
CASES=[
('id-substring-tip','714099a','--id-dir /home/otheruser/--id-dir --port 17200','--id-dir /home/otheruser/--id-dir --port 17200','/home/otheruser/--id-dir'),
('id-substring-baseline','110bb34','--id-dir /home/otheruser/--id-dir --port 17200','--id-dir /home/otheruser/--id-dir --port 17200','/home/otheruser/--id-dir'),
('id-equals-tip','714099a','--id-dir=/home/otheruser/--id-dir --port 17200','--id-dir=/home/otheruser/--id-dir --port 17200','/home/otheruser/--id-dir'),
('id-ordinary-control','714099a','--id-dir /home/otheruser/identity --port 17200','--id-dir /home/otheruser/identity --port 17200','/home/otheruser/identity'),
]
if selection: CASES=[c for c in CASES if c[0] in selection]
results=[]
for label,rev,running,configured,idpath in CASES:
 h=m.DeployInstallTests();h.setUp()
 try:
  h.text=subprocess.check_output(['git','show',rev+':scripts/deploy_linux.sh']).decode('utf8')
  h.make_private_discovery_model(main_exec='/opt/ak-sending/ak-cli registry-server '+configured,cmdline_extra=running,unit_service_lines=['User=otheruser','WorkingDirectory=/home/runtime-old'],environ=b'HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0')
  for path,key,db in [(idpath,'runtime-real-key','runtime-real-db'),('/home/runtime-old/--port','substring-decoy-key','substring-decoy-db')]:
   live=h.probe/path.lstrip('/');live.mkdir(parents=True,exist_ok=True)
   (live/'registry.key').write_text(key,encoding='utf8');(live/'registry.db').write_text(db,encoding='utf8')
  h._make_oldregistry_running()
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
  rc,stdout=h.run_driver(h.definitions()+m.BOX_MODELS,body,name='driver.sh',timeout=180)
  (h.probe/'driver.out').write_text(stdout,encoding='utf8')
  p1=(h.probe/'phase1.log').read_text(encoding='utf8');log=h.stubs_log()
  rec={'case':label,'revision':rev,'configured':configured,'running':running,'driver_rc':rc,'phase1_rc':int(re.search(r'PHASE1_RC=(\d+)',stdout)[1]),'stopped':bool(re.search(r'^systemctl stop ',log,re.M)),'refusals':[x for x in (p1+'\n'+stdout).splitlines() if 'ERROR:' in x or 'FAIL:' in x], 'phase1_runtime':[x for x in p1.splitlines() if 'runtime-id-dir:' in x]}
  for tag in ['DECISION','INSTALL']:
   x=re.search(tag+r'_RC=(\d+)',stdout)
   if x:rec[tag.lower()+'_rc']=int(x[1])
  unit=h.probe/'etc/systemd/system/ak-registry-server.service'
  if unit.exists():
   u=unit.read_text(encoding='utf8');rec['unit_fields']=[x for x in u.splitlines() if x.startswith(('ExecStart=','ReadWritePaths=','User=','WorkingDirectory='))]
   rec['old_active_captured']='oldregistry:exists:enabled:active' in (h.probe/'phase456.log').read_text(encoding='utf8')
   dest=h.probe/'opt-ak/data';rec['copied_key']=(dest/'registry.key').read_text(encoding='utf8') if (dest/'registry.key').exists() else None
   rec['copied_db']=(dest/'registry.db').read_text(encoding='utf8') if (dest/'registry.db').exists() else None
  target=OUT/label;shutil.copytree(h.probe,target,dirs_exist_ok=True)
  (target/'result.json').write_text(json.dumps(rec,indent=2),encoding='utf8')
  print(json.dumps(rec),flush=True);results.append(rec)
 finally:h.doCleanups()
(OUT/'results.json').write_text(json.dumps(results,indent=2),encoding='utf8')
cli=ROOT/'target/debug/ak-cli.exe'
assert cli.exists(), 'Build current CLI before safe parsing probes'
version=subprocess.check_output([str(cli),'--version'],text=True).strip()
cli_records=[]
# Original valid argv is parsed with the same literal basename and an
# invalid synthetic key, forcing identity loading to fail BEFORE listening.
guard=OUT/'parse-guard'/'--id-dir';guard.mkdir(parents=True,exist_ok=True)
(guard/'registry.key').write_bytes(b'synthetic-short')
for spelling in ['space','equals']:
 args=['registry-server']+(['--id-dir',str(guard.resolve())] if spelling=='space' else ['--id-dir='+str(guard.resolve())])+['--port','17200']
 p=subprocess.run([str(cli)]+args,cwd=OUT,capture_output=True,text=True,timeout=10)
 assert p.returncode==1 and 'seed file must hold exactly 32 bytes' in p.stderr,(spelling,p.returncode,p.stderr)
 cli_records.append({'case':'original-'+spelling,'version':version,'argv':args,'rc':p.returncode,'stderr':p.stderr})
for r in results:
 assert r['phase1_rc']==r['decision_rc']==r['install_rc']==0 and r['stopped'] and r['old_active_captured'],r
 if r['case']=='id-ordinary-control':
  assert r['copied_key']=='runtime-real-key' and r['copied_db']=='runtime-real-db',r
  assert r['phase1_runtime']==['  runtime-id-dir: /home/otheruser/identity'],r
  continue
 assert r['copied_key']=='substring-decoy-key' and r['copied_db']=='substring-decoy-db',r
 assert r['phase1_runtime']==['  runtime-id-dir: /home/runtime-old/--port'],r
 unit_exec=next(x[len('ExecStart='):] for x in r['unit_fields'] if x.startswith('ExecStart='))
 argv=shlex.split(unit_exec);argv[0]=str(cli)
 # The generated argv contains a stray absolute token and a bare 17200.
 # The permanently invalid appended port also guarantees no server spawn.
 argv+=['--port','never-a-valid-port']
 p=subprocess.run(argv,cwd=OUT,capture_output=True,text=True,timeout=10)
 assert p.returncode==2 and "unexpected argument '/home/runtime-old/--port'" in p.stderr,(r,p.returncode,p.stderr)
 cli_records.append({'case':r['case']+'-generated','version':version,'argv':argv,'rc':p.returncode,'stderr':p.stderr})
(OUT/'cli-parse.json').write_text(json.dumps(cli_records,indent=2),encoding='utf8')
print(json.dumps({'version':version,'cli_cases':len(cli_records),'evidence':str(OUT)},ensure_ascii=False),flush=True)
print('ALL_EXPECTATIONS_MATCHED',flush=True)
```

### 5.2 目录枚举的当前、基线与候选控制

```powershell
python <TEMP目录>/review_home_enumeration.py <新的证据目录>
```

```python
import importlib.util,json,re,shutil,subprocess,sys,time
from pathlib import Path
ROOT=Path.cwd(); OUT=Path(sys.argv[1]) if len(sys.argv)>1 else Path(__file__).parent/'home-enumeration';OUT.mkdir(parents=True,exist_ok=True)
spec=importlib.util.spec_from_file_location('m',ROOT/'scripts/tests/test_deploy_linux_install.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
results=[];start=time.monotonic()
CASES=[
 ('prefix-decoy-tip','714099a','other user','prefix'),
 ('prefix-decoy-baseline','110bb34','other user','prefix'),
 ('ordinary-control-tip','714099a','otheruser','none'),
 ('paired-candidate-tip','714099a','other user','candidate'),
 ('two-ordinary-tip','714099a','otherone','candidate'),
]
for label,rev,home,extra in CASES:
 h=m.DeployInstallTests();h.setUp()
 try:
  h.text=subprocess.check_output(['git','show',rev+':scripts/deploy_linux.sh']).decode('utf8')
  live=h.probe/'home'/home/'.ak-sending';live.mkdir(parents=True)
  (live/'registry.key').write_text('synthetic-existing-key',encoding='utf8');(live/'registry.db').write_text('synthetic-existing-db',encoding='utf8')
  physical_candidates=['/home/'+home+'/.ak-sending']
  if extra!='none':
   second=h.probe/('home/other' if extra=='prefix' else 'home/otheruser/.ak-sending');second.mkdir(parents=True)
   marker='synthetic-split-decoy' if extra=='prefix' else 'synthetic-ordinary'
   (second/'registry.key').write_text(marker+'-key',encoding='utf8');(second/'registry.db').write_text(marker+'-db',encoding='utf8')
   if extra=='candidate':physical_candidates.append('/home/otheruser/.ak-sending')
  # Preserve every model behavior except enumerate actual probe/home with actual GNU ls,
  # since the existing LS_HOME_ENTRIES stub independently word-splits whitespace names.
  ls=h.probe/'bin/ls';text=ls.read_text(encoding='utf8')
  real=m._posix_tool("ls")
  text=text.replace('for e in ${LS_HOME_ENTRIES:-otheruser}; do printf \'%s\\n\' "$e"; done', '"'+real+'" -1 "$PROBE_DIR/home"')
  assert '"'+real+'" -1 "$PROBE_DIR/home"' in text
  ls.write_text(text,encoding='utf8',newline='\n')
  h.driver_env['SS_EMPTY']='1'
  body=r"""
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
"""+h.decision_block(h.text,rev)+h.decisions_block(h.text,rev)+r"""
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
set +e
bash "$PROBE_DIR/phase456.sh" > "$PROBE_DIR/phase456.log" 2>&1
printf 'INSTALL_RC=%s\n' "$?"
"""
  rc,stdout=h.run_driver(h.definitions()+m.BOX_MODELS,body,name='driver.sh',timeout=180)
  (h.probe/'driver.out').write_text(stdout,encoding='utf8')
  p1=(h.probe/'phase1.log').read_text(encoding='utf8')
  unit=h.probe/'etc/systemd/system/ak-registry-server.service'
  rec={'case':label,'revision':rev,'home':home,'driver_rc':rc,'phase1_rc':int(re.search(r'PHASE1_RC=(\d+)',stdout)[1]),'data_reports':[x for x in p1.splitlines() if x.startswith('registry-data:')],'chosen':[x for x in stdout.splitlines() if x.startswith('CHOSEN')],'refusals':[x for x in stdout.splitlines() if 'FAIL:' in x]}
  for tag in ['DECISION','INSTALL']:
   mt=re.search(tag+r'_RC=(\d+)',stdout)
   if mt:rec[tag.lower()+'_rc']=int(mt[1])
  rec['physical_candidates']=physical_candidates
  log=h.stubs_log();rec['stopped']=bool(re.search(r'^systemctl stop ',log,re.M));rec['enabled_new_units']='systemctl enable --now ak-registry-server.service ak-stun-server.service' in log
  rec['source_key_unchanged']=(live/'registry.key').read_text()=='synthetic-existing-key'
  dest=h.probe/'opt-ak/data/registry.key';rec['destination_key']=dest.read_text() if dest.exists() else None
  destdb=h.probe/'opt-ak/data/registry.db';rec['destination_db']=destdb.read_text() if destdb.exists() else None
  if unit.exists():rec['unit_fields']=[x for x in unit.read_text().splitlines() if x.startswith(('ExecStart=','ReadWritePaths='))]
  shutil.copytree(h.probe,OUT/label,dirs_exist_ok=True)
  (OUT/label/'result.json').write_text(json.dumps(rec,indent=2),encoding='utf8')
  results.append(rec);print(json.dumps(rec),flush=True)
 finally:h.doCleanups()
(OUT/'results.json').write_text(json.dumps(results,indent=2),encoding='utf8')
for r in results:
 assert r['phase1_rc']==0 and not r['stopped'] and r['source_key_unchanged'],r
 if r['case']=='two-ordinary-tip':
  assert r['driver_rc']==1 and len(r['data_reports'])==2 and not r['enabled_new_units'] and 'install_rc' not in r,r
  assert 'multiple identity directories' in '\n'.join(r['refusals']),r
  assert r['destination_key'] is None and r['destination_db'] is None,r
 else:
  assert r['driver_rc']==r['decision_rc']==r['install_rc']==0 and r['enabled_new_units'],r
  if r['case'].startswith('prefix-decoy'):
   assert r['data_reports']==['registry-data: /home/other (key:yes db:yes)'],r
   assert 'CHOSEN_DATA_SOURCE=/home/other' in r['chosen'],r
   assert r['destination_key']=='synthetic-split-decoy-key' and r['destination_db']=='synthetic-split-decoy-db',r
  elif r['case']=='paired-candidate-tip':
   assert len(r['physical_candidates'])==2 and r['data_reports']==['registry-data: /home/otheruser/.ak-sending (key:yes db:yes)'],r
   assert r['destination_key']=='synthetic-ordinary-key' and r['destination_db']=='synthetic-ordinary-db',r
  else:
   assert r['data_reports']==['registry-data: /home/otheruser/.ak-sending (key:yes db:yes)'],r
   assert r['destination_key']=='synthetic-existing-key' and r['destination_db']=='synthetic-existing-db',r
print('ALL_EXPECTATIONS_MATCHED; elapsed='+str(time.monotonic()-start),flush=True)
```
