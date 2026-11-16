# 批次 K 修复轮十九独立复核（第 20 轮，2026-11-16）

> 历史归属补记（K21）：本报告及交接§45、STATUS、状态表在出具时仅本地同步，随后由执行方在 `f4b58a6` 纳入修复提交；`714099a` 只回填交接的一行哈希。本文以下保留出具时口径。

复核范围：`dev/batch-k-20261114` 的 `1d76871..110bb34`；实现与文档提交为 `a050315`，`110bb34` 只回填交接的一行提交号。固定 tip 为 `110bb3485cdee3d8bef1f64ae74bbbc2c95d42a0`。结论为 **REJECTED**：K19 的字面文件名误绑、尾部悬空与重复选项精确反例均已修复；新增预解析遍历却会执行 Bash 文件名展开，重新打开错库路径，显式空值等 CLI 无效形态也仍可进入停服。

逐项状态只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。命令、运行编号、计数、仓外材料及未执行边界只记在 [K 交接 §45](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#45-修复轮十九独立复核证据2026-11-16)。固定 tip CI 已通过 GitHub API 等到 completed/success；CI 成功不覆盖下面的行为反例。本轮只在本地同步审核文档，不修改被审实现、不创建提交、不合并、不推送、不删除分支。行号均指固定 tip。

## 1. K19 修复的实际效果

`scripts/deploy_linux.sh:1087–1103` 新增 `bound_db_arg()`，phase1 的配置/运行比较与相对 DB 的 cwd 判断改用它。decision 的 `:2619–2637` 另有按同一选项形状编写的遍历，并非所有阶段都调用同一个函数。对原始精确输入 `--db-path=--db-path --port 17200`，等号后的 `--db-path` 现在作为该 token 自有后缀，生成 unit 绑定 `/home/runtime-old/--db-path`，不再绑定后续 `--port`。指定回归、真实/诱饵 SQLite 断言及 `1d76871` 误绑、`4db2b4f` 停服前拒绝的历史对照实际执行通过。真实 tip CLI 也确认这个字面文件名可通过参数解析。

`:2638–2643` 已检查尾部 pending 状态和绑定数量。K19 的合法运行 argv 加尾部悬空独立 `--db-path`、以及重复 DB 选项的配置现在在 decision 拒绝，没有生成 CHOSEN 配置或 stop 调用。原单选项控制及独立补测的空格/等号混合顺序均保持。这里确认的是这些精确形态；下述空值与后继选项问题说明尚不能声称已覆盖真实 CLI 的全部缺值语义。

两点非阻断测试描述纠正：新 literal 回归先断言生成 unit 解析值等于真实绝对路径，随后用相同常量路径打开 SQLite，查询对象没有直接由解析结果变量构造，但前面的相等断言已约束二者一致。duplicate 回归的列表实际为“两空格、两等号同值、两等号异值”，并不包含注释所说的混合形式；独立补测混合方向均拒绝，未据此发现实现漏洞。

## 2. K20-N1 / P1：新增预解析执行 pathname expansion，字面文件名改成另一 DB 对象

unit 与运行 argv 完全一致，服务运行 cwd 为 `/home/runtime-old`：

```text
/opt/ak-sending/ak-cli registry-server --port 17200 --id-dir /home/otheruser/.ak-sending --db-path db[1].sqlite
```

真实 SQLite 是 `/home/runtime-old/db[1].sqlite`，含 `runtime-real`；诱饵 `/home/runtime-old/db1.sqlite` 含 `glob-decoy`。仅在本机部署 decision 的 cwd 放一个普通文件 `db1.sqlite`，就能改变生成结果。主复核把 phase1 放到另一个没有匹配文件的空 cwd，仍复现，因此不要求远端登录 cwd 与本机 cwd 一致，也没有伪造服务参数。

新增的 `:2623` 是未关闭 glob 的 `for re_tok in $REGISTRY_EXEC`。Bash 先做单词拆分和 pathname expansion，`:2625` 得到的原值已经从字面 `db[1].sqlite` 变成 `db1.sqlite`。`:2654` 的白名单看到的是展开后的安全字符；`:2669` 构造错误绝对路径；`:2673` 的改写遍历与后面的结果校验围绕同一错误 `abs_db` 工作，故全部通过。phase1 新 helper 的 `:1091` 也有同类展开，不是保留字节的 tokenizer。

实际完整离线链完成 phase1、decision、脚本生成与 install；捕获旧服务 enabled/active/PID/image，并调用 stop。生成 unit 指向：

```text
--db-path /home/runtime-old/db1.sqlite
```

按生成 unit 中实际提取的参数打开真实 SQLite，读到 `glob-decoy`，身份参数不变。等号形式也复现，部署 cwd 中对应匹配文件为 `--db-path=db1.sqlite`。无匹配文件时当前仍在白名单拒绝；普通 `db1.sqlite` 控制正确读取其运行库。直接基线 `1d76871` 对同样匹配条件在停服前拒绝，因为其 sed 预解析保留原方括号，白名单可见。

**归属：** `git blame` 将新增取值遍历定位到 `a050315:2619–2637`。旧改写循环也有未引用展开，但直接基线的白名单在到达它之前拒绝该输入；本轮把展开提前到白名单之前，形成已验证的新回归。不能只归为旧循环问题，也不能用现有“无匹配时方括号拒绝”的绿色测试否定此反例。

**修复要求：** 参数分词必须保留原始字节，不能依赖执行目录内容。先冻结原始 token，再识别选项、验证支持的字符与 CLI 形状、推导绝对对象并核验输出。在明确作用域禁用 pathname expansion 或用不执行 glob 的数组解析均可；不支持字面方括号路径时应继续停服前拒绝。回归需保留有/无匹配、两种拼写、phase1 与本机 cwd 分离及按生成参数读取真实/诱饵 SQLite 的对照。

## 3. K20-N2 / P2：显式空值及下一选项仍被当成可接受的 DB 绑定

主反例保持合法运行 argv 使用默认库，仅将运行后的 unit 编辑为：

```text
运行：registry-server --id-dir /home/otheruser/.ak-sending --port 17200
配置：registry-server --id-dir /home/otheruser/.ak-sending --port 17200 --db-path=
```

`:1089–1102` 用空字符串同时表示选项缺席与显式空后缀，`:1142–1143` 因此把非法空值回退为默认库，比较相等。decision 的 `:2633–2635` 只累计绑定个数；`:2638–2643` 既无尾部 pending 又无重复，随后 `:2645–2646` 的空值分支直接跳过改写/验证。

主复核与专项都实际执行完整链，当前与直接基线均经过停服，生成 unit 仍带 `--db-path=`。用本轮实际构建、版本标记 git `110bb34` 的 CLI 检查生成参数，得到 rc 2：`a value is required for '--db-path <DB_PATH>' but none was supplied`。检查把永久无效的 port 放在最后，保证不能启动服务，并断言首先报具体 DB 错误。由 `1d76871` 对照可知，这是本轮发现且新增校验未覆盖的既有缺口，不称本轮新引入。

补充形态揭示同一解析契约问题：合法运行参数 `--db-path=--port --port 17200`，只把配置改为 `--db-path --port 17200`。当前空格形式取“任何下一 token”作为值，错误地把选项 `--port` 消费为文件名，phase1 比较相等；完整链生成 `--db-path /home/runtime-old/--port 17200` 并停服，余下 `17200` 成为意外位置参数。真实 tip CLI 对原配置报 DB 缺值，对生成参数报 `unexpected argument '17200' found`。当前 `main.rs:164–166` 为普通 `#[arg(long)] db_path: Option<PathBuf>`，没有开启允许后继连字符选项作为空格值的配置。此补充只执行了当前完整链，不据此推断它的引入版本。

**修复要求：** 解析结果必须区分 absent、合法非空绑定、显式空值、缺值及重复绑定；空格形式需遵循实际 CLI 对后继选项的规则，不能把“下一 token 存在”等同“合法值存在”。任何无法证明可接受的配置应停服前拒绝。保留合法运行 argv 加非法 unit 的完整链测试，同时保留等号形式字面连字符文件名的合法控制。

## 4. 证据边界与可重放代码

下列 runner 从固定 git 版本取真实生成器和决策，只把 proc、systemctl、权限及故障接口替换成仓库现有离线模型。Bash、生成脚本、文件操作与 SQLite 查询实际执行；不能把 install 模型成功说成原生 Linux systemd 成功启动。CLI 合法值探针在 TEMP 中预置无效长度的合成 key，使身份加载在配置和监听之前失败；非法参数探针使用永久无效 port 保证不启动服务。没有生产访问、SSH、物理双机、抓包、relay 或真实用户数据。

将各段代码保存到仓外新的 TEMP 目录，从仓库根运行。`ALL_EXPECTATIONS_MATCHED` 是缺陷及对照符合断言，不是产品通过。固定提交需存在于本地历史。CLI 部分需要 `cargo build --locked --workspace` 生成当前 Windows debug 程序；其他平台须按本机二进制名称调整。

### 4.1 本机 cwd 有匹配、phase1 cwd 隔离的错库对照

```powershell
python <TEMP目录>/review_glob_isolated.py
```

```python
import importlib.util,json,re,shlex,shutil,sqlite3,subprocess,sys,time
from pathlib import Path
ROOT=Path.cwd()
OUT=Path(__file__).parent/'isolated-phase1-cwd'
OUT.mkdir(parents=True,exist_ok=True)
spec=importlib.util.spec_from_file_location('m',ROOT/'scripts/tests/test_deploy_linux_install.py')
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
CASES=[('space-match-tip','110bb34',' ','db[1].sqlite',True),('space-match-baseline','1d76871',' ','db[1].sqlite',True),('space-no-match-tip','110bb34',' ','db[1].sqlite',False),('equals-match-tip','110bb34','=','db[1].sqlite',True),('equals-match-baseline','1d76871','=','db[1].sqlite',True),('ordinary-tip','110bb34',' ','db1.sqlite',False)]
if len(sys.argv)>1: CASES=[c for c in CASES if c[0] in sys.argv[1:]]
results=[];start=time.monotonic()
for label,rev,sep,value,match in CASES:
 h=m.DeployInstallTests();h.setUp()
 try:
  h.text=subprocess.check_output(['git','show',rev+':scripts/deploy_linux.sh']).decode('utf8')
  identity='/home/otheruser/.ak-sending'
  args='--port 17200 --id-dir '+identity+' --db-path'+sep+value
  h.make_private_discovery_model(main_exec='/opt/ak-sending/ak-cli registry-server '+args,cmdline_extra=args,unit_service_lines=['User=otheruser','WorkingDirectory=/home/runtime-old'],environ=b'HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0')
  live=h.probe/identity.lstrip('/');live.mkdir(parents=True,exist_ok=True)
  (live/'registry.key').write_text('synthetic-runtime-key',encoding='utf8')
  (live/'registry.db').write_text('synthetic-identity-default-db',encoding='utf8')
  objects=[('/home/runtime-old/'+value,'runtime-real')]
  if value!='db1.sqlite':objects.append(('/home/runtime-old/db1.sqlite','glob-decoy'))
  for obj,marker in objects:
   path=h.probe/obj.lstrip('/');path.parent.mkdir(parents=True,exist_ok=True)
   con=sqlite3.connect(path);con.execute('CREATE TABLE review_marker(value TEXT)');con.execute('INSERT INTO review_marker VALUES (?)',(marker,));con.commit();con.close()
  if match:(h.probe/(('--db-path=' if sep=='=' else '')+'db1.sqlite')).write_text('synthetic-shell-glob-match',encoding='utf8')
  state=h.probe/'systemctl-state';state.mkdir(parents=True,exist_ok=True)
  for suffix,value_ in [('enabled','1'),('active','active'),('pid','4242')]:
   (state/('oldregistry.service.'+suffix)).write_text(value_+'\n',encoding='utf8')
  (state/'4242.exe').write_text('/opt/ak-sending/ak-cli\n',encoding='utf8')
  (state/'oldregistry.service.running').write_text('/opt/ak-sending/ak-cli\n',encoding='utf8')
  (state/'oldregistry.service.running.bin').write_text(m.AK_CLI_OLD,encoding='utf8')
  h.driver_env['PROC_CWD_MAP']='/proc/4242/cwd=/home/runtime-old'
  body=r'''
WORK="$PROBE_DIR"
KEEP_LEGACY_SIGNAL=0
CHECK_ONLY=0
write_phase1 "$PROBE_DIR/phase1.sh" || exit 90
mkdir -p "$PROBE_DIR/preflight-cwd"
(cd "$PROBE_DIR/preflight-cwd" && bash "$PROBE_DIR/phase1.sh") > "$PROBE_DIR/phase1.log" 2>&1
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
  rec={'case':label,'revision':rev,'args':args,'glob_match':match,'driver_rc':rc,'phase1_rc':int(re.search(r'PHASE1_RC=(\d+)',stdout)[1]),'stopped':bool(re.search(r'^systemctl stop ',log,re.M)),'refusals':[x for x in stdout.splitlines() if 'FAIL:' in x]}
  for tag in ['DECISION','INSTALL']:
   x=re.search(tag+r'_RC=(\d+)',stdout)
   if x:rec[tag.lower()+'_rc']=int(x[1])
  unit=h.probe/'etc/systemd/system/ak-registry-server.service'
  if unit.exists():
   u=unit.read_text(encoding='utf8');rec['unit_fields']=[x for x in u.splitlines() if x.startswith(('ExecStart=','ReadWritePaths=','User=','WorkingDirectory='))]
   ex=re.search(r'^ExecStart=(.*)$',u,re.M)[1];argv=shlex.split(ex)
   db=next((x.split('=',1)[1] for x in argv if x.startswith('--db-path=')),None)
   if db is None:db=argv[argv.index('--db-path')+1]
   rec['generated_db_arg']=db
   con=sqlite3.connect(h.probe/db.lstrip('/'));rec['selected_db_marker']=con.execute('SELECT value FROM review_marker').fetchone()[0];con.close()
   rec['old_active_captured']='oldregistry:exists:enabled:active' in (h.probe/'phase456.log').read_text(encoding='utf8')
  target=OUT/label;shutil.copytree(h.probe,target,dirs_exist_ok=True)
  (target/'result.json').write_text(json.dumps(rec,indent=2),encoding='utf8')
  print(json.dumps(rec),flush=True);results.append(rec)
 finally:h.doCleanups()
(OUT/'results.json').write_text(json.dumps(results,indent=2),encoding='utf8')
for r in results:
 if 'baseline' in r['case'] or 'no-match' in r['case']:
  assert r['phase1_rc']==0 and r['driver_rc']==1 and not r['stopped'] and 'install_rc' not in r,r
  assert 'contains bytes' in '\n'.join(r['refusals']),r
 else:
  assert r['phase1_rc']==r['decision_rc']==r['install_rc']==0 and r['stopped'] and r['old_active_captured'],r
  assert r['generated_db_arg']=='/home/runtime-old/db1.sqlite',r
  assert r['selected_db_marker']==('runtime-real' if r['case']=='ordinary-tip' else 'glob-decoy'),r
print('ALL_EXPECTATIONS_MATCHED; elapsed='+str(time.monotonic()-start),flush=True)
```

### 4.2 显式空值的当前/基线与真实 tip CLI

```powershell
python <另一TEMP目录>/review_empty_db.py
```

```python
import importlib.util,json,re,shlex,shutil,sqlite3,subprocess,sys,time
from pathlib import Path
ROOT=Path.cwd();OUT=Path(__file__).parent
spec=importlib.util.spec_from_file_location('deploy_tests',ROOT/'scripts/tests/test_deploy_linux_install.py')
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
identity='/home/otheruser/.ak-sending'
CASES=[('empty-tip','110bb34','--id-dir '+identity+' --port 17200','--id-dir '+identity+' --port 17200 --db-path='),('option-value-tip','110bb34','--id-dir '+identity+' --db-path=--port --port 17200','--id-dir '+identity+' --db-path --port 17200'),('empty-baseline','1d76871','--id-dir '+identity+' --port 17200','--id-dir '+identity+' --port 17200 --db-path=')]
CASES=[c for c in CASES if c[0] in ('empty-tip','empty-baseline')]
results=[]
for label,rev,running,configured in CASES:
 h=m.DeployInstallTests();h.setUp()
 try:
  h.text=subprocess.check_output(['git','show',rev+':scripts/deploy_linux.sh']).decode('utf8')
  h.make_private_discovery_model(main_exec='/opt/ak-sending/ak-cli registry-server '+configured,cmdline_extra=running,unit_service_lines=['User=otheruser','WorkingDirectory=/home/runtime-old'],environ=b'HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0')
  live=h.probe/identity.lstrip('/');live.mkdir(parents=True,exist_ok=True)
  (live/'registry.key').write_text('synthetic-runtime-key',encoding='utf8')
  (live/'registry.db').write_text('synthetic-runtime-db',encoding='utf8')
  db=h.probe/'home/runtime-old/--port';db.parent.mkdir(parents=True,exist_ok=True)
  con=sqlite3.connect(db);con.execute('CREATE TABLE review_marker(value TEXT)');con.execute('INSERT INTO review_marker VALUES (?)',('runtime-real',));con.commit();con.close()
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
  rec={'case':label,'revision':rev,'configured':configured,'running':running,'driver_rc':driver_rc,'phase1_rc':int(re.search('PHASE1_RC=(\\d+)',stdout)[1]),'stopped':bool(re.search(r'^systemctl stop ',log,re.M))}
  rec['refusal_lines']=[x for x in stdout.splitlines() if 'FAIL:' in x]
  dec=re.search('DECISION_RC=(\\d+)',stdout);ins=re.search('INSTALL_RC=(\\d+)',stdout)
  if dec:rec['decision_rc']=int(dec[1])
  if ins:rec['install_rc']=int(ins[1])
  unit=h.probe/'etc/systemd/system/ak-registry-server.service'
  if unit.exists():
   u=unit.read_text(encoding='utf8');rec['unit_fields']=[x for x in u.splitlines() if x.startswith(('ExecStart=','ReadWritePaths=','User=','WorkingDirectory='))]
   ex=re.search(r'^ExecStart=(.*)$',u,re.M)[1]
   parsed=shlex.split(ex);parsed[0]=str((ROOT/'target/debug/ak-cli.exe').resolve())
   if '--port' in parsed:
    pi=parsed.index('--port');del parsed[pi:pi+2]
   parsed.extend(['--port','never-a-valid-port'])
   actual=subprocess.run(parsed,cwd=h.probe,capture_output=True,text=True,timeout=10)
   rec['cli_rc']=actual.returncode;rec['cli_stderr']=actual.stderr
   assert actual.returncode==2,rec
  dest=OUT/label;shutil.copytree(h.probe,dest,dirs_exist_ok=True)
  (dest/'result.json').write_text(json.dumps(rec,indent=2),encoding='utf8')
  results.append(rec);print(json.dumps(rec),flush=True)
 finally:h.doCleanups()
(OUT/'results.json').write_text(json.dumps(results,indent=2),encoding='utf8')
print('EVIDENCE='+str(OUT),flush=True)

for r in results:
 assert r["phase1_rc"]==r["decision_rc"]==r["install_rc"]==0 and r["stopped"],r
 assert r["cli_rc"]==2 and "a value is required" in r["cli_stderr"],r
print("ALL_EXPECTATIONS_MATCHED",flush=True)
```
