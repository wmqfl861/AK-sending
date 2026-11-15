# 批次 K 修复轮八独立复核（2026-11-15）

复核范围：`dev/batch-k-20261114` 的 `12a71c9..ccd8fb2`；代码与文档修复为 `7e8bfb2`，`ccd8fb2` 仅作哈希回填。被审 tip 固定为 `ccd8fb246dc96aaa3fd21233946327fa04171170`。结论为 **REJECTED**：K8 的精确默认数据库反例已修复，但新发现 K9-N1——双方省略身份目录时，后续迁移来源仍由目录枚举首项决定，可能选中与运行服务无关的身份和数据库。该缺陷早于本轮存在，不归为本轮新引入回归，也不撤回 K8 精确修复的通过判断。

逐项状态唯一记录在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。实际命令、计数、GitHub 运行编号、材料位置与未执行项集中在 [K 交接 §23](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#23-修复轮八独立复核证据2026-11-15)。本轮出具时仅作本地报告和状态同步，不修改被审代码、不提交、不合并或推送 main、不删除批次分支；报告及同步随后由执行方在 `e03a44f` 提交。下列行号均指固定 tip。

已使用当前 git 凭据直接查询 GitHub run/jobs API，从 in_progress 等待至 completed/success；完整 head SHA 与固定候选一致，全部常规 jobs 及其门槛步骤成功。本机门禁也已完成；这些结果不覆盖下述新增的独立行为反例。

## 1. 新阻断发现

### K9-N1 / P1：双方省略身份目录时，迁移来源没有绑定到运行服务的实际身份

`scripts/deploy_linux.sh:1220` 按 `$REMOTE_DATA`、`/root/.ak-sending`、各 home 的顺序发现身份目录，`:1896` 用 `head -1` 无条件选取首个 `registry-data`。此选择没有绑定到已识别的端口占用者、该进程的有效身份目录或其所属用户。

这在旧配置显式保留 `--id-dir` 时不一定改变服务使用的身份；但双方均省略该参数时，`:467` 的 `compose_registry_exec` 会给新 unit 补上 `--id-dir $REMOTE_DATA`，`:2015` 又把上述首项赋给 `DATA_SOURCE_DIR`。随后 `:1366`–`:1373` 的迁移逻辑会从该来源复制 key/db。因而“比较通过”与“迁移了该运行服务的数据”之间仍缺少绑定。

确定输入为一个已识别的 `oldregistry.service` 占用注册端口，unit 配置如下：

```ini
[Service]
User=otheruser
Environment=HOME=/home/otheruser
UnsetEnvironment=USERPROFILE
ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200
```

运行 `/proc/4242/cmdline` 是与 ExecStart 相同的逐参数 NUL argv；合成 `/proc/4242/environ` 含 `HOME=/home/otheruser`，没有 `USERPROFILE`。`/home/otheruser/.ak-sending` 内已有合成 `registry.key`、`registry.db`，目标 `$REMOTE_DATA` 没有旧数据。根据 `crates/ak-cli/src/main.rs:709`–`:715`，运行身份唯一确定为该 home 下的 `.ak-sending`；数据库由 `:3777`–`:3778`、`:3792` 确定为其中的 `registry.db`。这里没有假定双方使用不同环境：它们确实共享同一默认值。

只增加一份无关的 `/root/.ak-sending/registry.key` 和 `registry.db`，保持 unit、运行 argv、运行环境及原身份目录不变。候选的真实 phase1、真实 preflight decision、phase 4–6 decisions 和 compose 链输出：

```text
RUN_RC=0
registry-data: /root/.ak-sending (key:yes db:yes)
registry-data: /home/otheruser/.ak-sending (key:yes db:yes)
DECISION_RC=0
CHOSEN_EXEC=<REMOTE_BIN> registry-server --port 17200 --id-dir <REMOTE_DATA>
CHOSEN_DATA_SOURCE=/root/.ak-sending
CHOSEN_RW=<REMOTE_DATA>
CHOSEN_OLD_REGISTRY=oldregistry
DECISION_DONE
```

删除额外的合成 root 数据后，相同链仍成功，但 `CHOSEN_DATA_SOURCE` 恢复为 `/home/otheruser/.ak-sending`。无关目录是否存在不应决定这个已识别运行服务的身份来源。集成者与专项各自独立重放；`12a71c9` 和 `bb1f673` 的对照也有该行为，证明这是此前已存在、在本轮确认的缺口。

**判断边界。** 双方共享 HOME 时，`reg_args_agree` 对两个隐式值判相等本身没有错；本发现不要求把所有双省略情况认定为实质分歧。错误在后续未确定实际身份来源，却以枚举首项准备迁移到新显式目录。K2-R3 既有要求是无法确定身份或数据库时在切换前明确拒绝；当前链没有做到这一点。

**修复要求。** 将运行实例的实际身份与数据库来源贯穿到保留/迁移决策；能可靠确定时使用对应来源，无法确定或存在未消解歧义时拒绝。不得由目录枚举顺序推断归属。回归应保留明确的服务用户与运行环境、逐参数 NUL argv、正确旧数据，并仅增减无关 root/home/目标目录数据作为对照，执行到真实配置选择；同时保留本轮已通过的默认数据库等价与冲突输入。

以上只验证离线模型中的真实脚本配置选择，没有安装新 unit、启动真实 registry 或打开生产数据库，不声称真实身份已经轮换或数据已经丢失。

## 2. 已翻转输入、保留行为与非阻断纠正

K8 的精确输入（显式相同 id-dir，运行省略 db-path，配置指定 new.db）现在在 phase1 退出 3，报告 `materially disagrees`；真实后续决策退出 1，没有 `CHOSEN_` 输出。仅将运行默认数据库显式化的冲突也拒绝。双方省略数据库、运行隐式/配置显式等价默认，以及相反方向的等价输入均保留原数据库语义；相反方向在上一候选中的保守误拒绝也已消除。旧 git 基线仍实际执行至选择 new.db。

默认值已逐项核对 CLI 源码：数据库由有效 id-dir 派生，身份目录依次依赖 USERPROFILE、HOME、当前目录，端口与 bind 使用源码常量默认。实际生成函数的返回契约为 0/1/2；单侧身份默认无法从 argv 确定时返回 2，实质分歧返回 1，最终占用者 caller 对二者均退出 3、消息不同。完整 `--bind IP:PORT` 与独立 port 的语义等价尚未全部归一化，实测表现为保守拒绝，本轮不据此另列阻断。

K2-R4 的尖括号目标 LF、CR、CRLF、空行及 fragment 换行输入，经完整导出树检查均明确拒绝并携带完整目标。合法 canonical 目标、fragment 和多行 title 的通过对照保留；archive-space 目标继续被准确拒绝。CommonMark 0.31.2 的目标/标题规则与本轮核心实现一致，未发现这一检查器改动带来的新阻断。

两处源码注释残留需单独说明，未据此认定代码失败：部署脚本 `:987`–`:988` 仍写省略的运行参数不参与比较，与新实现相反；检查器 `:368`、`:371` 附近仍泛称所有 handoff 形态不会被忽略、backstop 使用相同目标语法，而顶部已准确限定范围且 backstop 实际更宽容。受支持范围之外的形式不应被宣称全部拒绝。

链接计数必须继续按文档树归属区分。执行方 §22 的“工作树”数值属于 K8 本地同步时点，不代表本轮固定候选；固定树计数及多代正则的相等结果统一见交接证据表。本轮不将此来源纠正列为代码阻断。

`7e8bfb2` 确已提交 K8 报告、交接与状态同步；`ccd8fb2` 确实只回填提交号。候选仍残留“K8 仅本地”与旧的最新验证入口，本轮改为历史归属并同步 K9。K3-N1、K2-N1、K2-R2、K2-R5 及此前已接受的其他结论保留；门禁结果与新行为反例分别记录，绿色 CI 不代替缺失的身份来源验证。

## 3. 可复现步骤与后续处理

在仓库根目录用 PowerShell 执行以下最小重放。使用既有离线 harness，所有特权路径映射到临时目录，key/db 内容均为合成文本，不连接生产：

```powershell
@'
import importlib.util
from pathlib import Path
import subprocess

spec = importlib.util.spec_from_file_location(
    "deploy_tests", Path("scripts/tests/test_deploy_linux_install.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
base = "/opt/ak-sending/ak-cli registry-server --port 17200"
for extra_root in (False, True):
    h = m.DeployInstallTests()
    h.setUp()
    try:
        h.text = subprocess.check_output(
            ["git", "show", "ccd8fb2:scripts/deploy_linux.sh"]).decode("utf-8")
        h.make_private_discovery_model(main_exec=base)
        unit = h.probe / "etc/systemd/system/oldregistry.service"
        unit.write_text(unit.read_text().replace(
            "[Service]\n",
            "[Service]\nUser=otheruser\nEnvironment=HOME=/home/otheruser\n"
            "UnsetEnvironment=USERPROFILE\n"), encoding="utf-8", newline="\n")
        (h.probe / "proc/4242/cmdline").write_bytes(
            b"\0".join(a.encode() for a in base.split()) + b"\0")
        (h.probe / "proc/4242/environ").write_bytes(
            b"HOME=/home/otheruser\0USER=otheruser\0")
        if extra_root:
            d = h.probe / "root/.ak-sending"
            d.mkdir(parents=True)
            (d / "registry.key").write_text("unrelated-synthetic-root-key")
            (d / "registry.db").write_text("unrelated-synthetic-root-db")
        rc, out = h.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || exit 90\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        phase = (h.probe / "p1.out").read_text(encoding="utf-8")
        decision_rc, decision = h.run_decision_and_config(phase, "decision.sh")
        print("EXTRA_ROOT=", extra_root, "HARNESS_RC=", rc)
        print(out, phase, "DECISION_RC=", decision_rc, decision)
    finally:
        h.doCleanups()
'@ | python -
```

两组 phase1/decision 均为 0/0。无额外 root 数据时迁移源为正确 home，增加无关 root 数据后迁移源变成 root；新 unit 两次都被补上 REMOTE_DATA。runner 自身 exit 0 只表示重放完成，应读取阶段退出值和 CHOSEN 输出。

后续在现有批次分支修复运行身份到迁移来源的绑定，加入上述行为回归，再交独立复核。Linux 原生 systemd/权限部署、物理双机与生产抓包仍未执行；本机测试、平台 CI 和离线模型不代替这些验收。ADR-001、无 relay、无隐藏代理回退和不压缩/不转码约束保持不变。
