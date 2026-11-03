//! `ak-cli doctor`: environment self-check for two-machine LAN debugging
//! (REVIEW 2026-11-02 4.3 -- desktop firewall prompts, AP isolation).
//!
//! Every probe is bounded by a timeout and reports ok/warn/skip instead of
//! failing the command: doctor exists to explain the environment, not to
//! gate it. `--json` emits the same checks machine-readable.
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;

use ak_core::handle::SaveTarget as _;
use ak_identity::{DeviceKeyPair, Fingerprint};

use crate::{human_bytes, json_escape, load_peer_book, peer_book_path, whoami};

/// Outer budget for one UDP bind (the bind itself is instant; the timeout
/// exists so a wedged network stack cannot hang doctor).
const BIND_TIMEOUT: Duration = Duration::from_secs(2);
/// STUN query budget per the task spec (retransmits stay inside it).
const STUN_TIMEOUT: Duration = Duration::from_secs(3);
/// Extra wall-clock slack around the sync mDNS round (daemon start +
/// goodbye) on top of the browse window.
const MDNS_SLACK: Duration = Duration::from_secs(5);
/// Nominal transfer port the mDNS probe advertises (discovery publishes the
/// number only; nothing listens here).
const MDNS_PROBE_PORT: u16 = 7100;

/// Outcome of one doctor check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckStatus {
    /// Probe ran and passed.
    Ok,
    /// Probe ran but found something worth flagging.
    Warn,
    /// Probe not applicable in this environment (reason in the detail).
    Skip,
}

impl CheckStatus {
    fn as_str(self) -> &'static str {
        match self {
            CheckStatus::Ok => "ok",
            CheckStatus::Warn => "warn",
            CheckStatus::Skip => "skip",
        }
    }
}

/// One doctor check line: status, headline detail, optional sub-lines and
/// advice. Kept string-shaped so probes stay trivially testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckResult {
    pub(crate) name: &'static str,
    pub(crate) status: CheckStatus,
    pub(crate) detail: String,
    pub(crate) extra: Vec<String>,
    pub(crate) advice: Option<String>,
}

impl CheckResult {
    fn new(name: &'static str, status: CheckStatus, detail: String) -> Self {
        CheckResult {
            name,
            status,
            detail,
            extra: Vec::new(),
            advice: None,
        }
    }

    fn with_extra(mut self, extra: Vec<String>) -> Self {
        self.extra = extra;
        self
    }

    fn with_advice(mut self, advice: &str) -> Self {
        self.advice = Some(advice.to_owned());
        self
    }
}

// ---------------------------------------------------------------------------
// Probes
// ---------------------------------------------------------------------------

/// Device identity: load-or-create the key, show the short fingerprint, and
/// count peer book entries. Returns the loaded fingerprint so the mDNS probe
/// advertises the real identity (None when the key itself failed).
fn probe_identity(id: &Path) -> (CheckResult, Option<Fingerprint>) {
    let key_path = id.join("device.key");
    match DeviceKeyPair::load_or_create(&key_path) {
        Ok((key, created)) => {
            let fingerprint = key.fingerprint();
            let state = if created { "本次新建" } else { "已存在" };
            let result = match load_peer_book(&peer_book_path(id)) {
                Ok(book) => {
                    let count = book.iter().count();
                    let result = CheckResult::new(
                        "identity",
                        CheckStatus::Ok,
                        format!(
                            "device.key {state}；指纹 {}；配对簿 {count} 条（{}）",
                            fingerprint.short_id(),
                            peer_book_path(id).display()
                        ),
                    );
                    if count == 0 {
                        result.with_advice(
                            "配对簿为空：两机联调前先在两端 `ak-cli pair code` 并互相 \
                             `ak-cli pair add <指纹> --name 名称`",
                        )
                    } else {
                        result
                    }
                }
                Err(err) => CheckResult::new(
                    "identity",
                    CheckStatus::Warn,
                    format!(
                        "device.key {state}；指纹 {}；配对簿不可读：{err:#}",
                        fingerprint.short_id()
                    ),
                )
                .with_advice(
                    "peers.book 损坏时 send/receive 的默认配对策略会失效；备份后重建该文件",
                ),
            };
            (result, Some(fingerprint))
        }
        Err(err) => (
            CheckResult::new(
                "identity",
                CheckStatus::Warn,
                format!("无法加载/新建 device.key（{}）：{err}", key_path.display()),
            )
            .with_advice("检查 id 目录权限；密钥不可用时 send/receive 均无法工作"),
            None,
        ),
    }
}

/// `addr/prefix` display of one local network.
fn net_text(net: &ak_net::classify::Net) -> String {
    format!("{}/{}", net.addr, net.prefix_len)
}

/// Split local nets into display strings by family (v4 first, input order
/// kept within each family).
fn format_nets(nets: &[ak_net::classify::Net]) -> (String, String) {
    let v4: Vec<String> = nets
        .iter()
        .filter(|net| net.addr.is_ipv4())
        .map(net_text)
        .collect();
    let v6: Vec<String> = nets
        .iter()
        .filter(|net| net.addr.is_ipv6())
        .map(net_text)
        .collect();
    (v4.join(", "), v6.join(", "))
}

/// Local interface networks (v4 and v6 listed separately).
fn probe_local_nets() -> CheckResult {
    match ak_net::classify::local_nets() {
        Ok(nets) => {
            let (v4, v6) = format_nets(&nets);
            if nets.is_empty() {
                return CheckResult::new(
                    "local-nets",
                    CheckStatus::Warn,
                    "未枚举到任何本地网卡网络".to_owned(),
                )
                .with_advice(
                    "VPN/虚拟网卡环境可能隐藏物理网卡；联调可改用手动 IP（send --to 直接指定对端地址）",
                );
            }
            CheckResult::new(
                "local-nets",
                CheckStatus::Ok,
                format!(
                    "{} 个本地网络（v4 {} 个，v6 {} 个）",
                    nets.len(),
                    v4.len(),
                    v6.len()
                ),
            )
            .with_extra(vec![
                format!("v4: {}", non_empty_or(v4)),
                format!("v6: {}", non_empty_or(v6)),
            ])
        }
        Err(err) => CheckResult::new(
            "local-nets",
            CheckStatus::Warn,
            format!("枚举本地网络失败：{err}"),
        )
        .with_advice(
            "网卡枚举被拒（安全软件/权限）；联调可改用手动 IP（send --to 直接指定对端地址）",
        ),
    }
}

fn non_empty_or(list: String) -> String {
    if list.is_empty() {
        "(无)".to_owned()
    } else {
        list
    }
}

/// Bind one wildcard UDP socket under a timeout, returning its local addr.
async fn bind_one(addr: &str) -> std::io::Result<String> {
    let socket = tokio::time::timeout(BIND_TIMEOUT, tokio::net::UdpSocket::bind(addr))
        .await
        .map_err(std::io::Error::other)??;
    Ok(socket.local_addr()?.to_string())
}

/// UDP bind sanity: one IPv4 and one IPv6 wildcard socket (the transport
/// both engines use; nothing is sent or received here).
async fn probe_udp_bind() -> CheckResult {
    let v4 = bind_one("0.0.0.0:0").await;
    let v6 = bind_one("[::]:0").await;
    match (&v4, &v6) {
        (Ok(v4), Ok(v6)) => CheckResult::new(
            "udp-bind",
            CheckStatus::Ok,
            format!("双栈 UDP 绑定成功：{v4} 与 {v6}"),
        ),
        (Ok(v4), Err(err6)) => CheckResult::new(
            "udp-bind",
            CheckStatus::Warn,
            format!("IPv4 绑定成功（{v4}）；IPv6 绑定失败：{err6}"),
        )
        .with_advice("本机可能未启用 IPv6；IPv4 局域网直连不受影响"),
        (Err(err4), Ok(v6)) => CheckResult::new(
            "udp-bind",
            CheckStatus::Warn,
            format!("IPv6 绑定成功（{v6}）；IPv4 绑定失败：{err4}"),
        )
        .with_advice("IPv4 UDP 被拦截（安全软件常见）；两机需至少一个可用地址族"),
        (Err(err4), Err(err6)) => CheckResult::new(
            "udp-bind",
            CheckStatus::Warn,
            format!("IPv4/IPv6 UDP 绑定均失败：{err4} / {err6}"),
        )
        .with_advice(
            "检查安全软件对 UDP 的限制；Windows 首次监听防火墙弹窗应选择“允许”\
             （仅开放本直连传输端口）",
        ),
    }
}

/// Best-effort mDNS round: advertise this device, browse briefly, expect to
/// see our own service back via multicast loopback. Every environment
/// failure (daemon, advertise, browse) is a SKIP with the reason; a browse
/// that runs but never sees us is a WARN pointing at multicast/AP isolation.
fn probe_mdns(fingerprint: &Fingerprint, device_name: &str, seconds: u64) -> CheckResult {
    let mut daemon = match ak_discovery::MdnsDiscovery::new() {
        Ok(daemon) => daemon,
        Err(err) => {
            return CheckResult::new(
                "mdns",
                CheckStatus::Skip,
                format!("mDNS 守护进程不可用：{err}"),
            )
            .with_advice(
                "组播被禁用或平台权限缺失（iOS/macOS 本地网络授权、Android MulticastLock）；\
                 改用手动 IP（send --to）或公网 probe 流程",
            );
        }
    };
    if let Err(err) = daemon.advertise(MDNS_PROBE_PORT, fingerprint, device_name) {
        return CheckResult::new("mdns", CheckStatus::Skip, format!("mDNS 广播失败：{err}"))
            .with_advice(
                "组播可能被网络屏蔽（AP 隔离/公司防火墙）；两机联调可改用手动 IP（send --to）",
            );
    }
    let found = match daemon.browse(Duration::from_secs(seconds)) {
        Ok(found) => found,
        Err(err) => {
            daemon.stop_advertise();
            return CheckResult::new("mdns", CheckStatus::Skip, format!("mDNS 浏览失败：{err}"))
                .with_advice("组播环境不可用；改用手动 IP（send --to）或公网 probe 流程");
        }
    };
    daemon.stop_advertise();
    let total = found.len();
    if found
        .iter()
        .any(|candidate| candidate.fingerprint == Some(*fingerprint))
    {
        CheckResult::new(
            "mdns",
            CheckStatus::Ok,
            format!("广播+回环浏览成功：{seconds}s 内发现 {total} 个 _ak-sending 服务（含本机）"),
        )
    } else {
        CheckResult::new(
            "mdns",
            CheckStatus::Warn,
            format!("浏览完成但未见本机服务（发现 {total} 个其他服务）"),
        )
        .with_advice(
            "组播回环可能被禁用；若 `ak-cli discover` 中两台设备互相看不到，优先怀疑 \
             AP 隔离（酒店/访客/公司 Wi-Fi 常见），此时改用手动 IP（send --to）或公网 probe 流程",
        )
    }
}

/// Optional STUN mapping query. No `--stun` means SKIP by design: the check
/// touches the public internet and is irrelevant for LAN-only debugging.
async fn probe_stun(server: Option<SocketAddr>, timeout: Duration) -> CheckResult {
    let Some(server) = server else {
        return CheckResult::new(
            "stun",
            CheckStatus::Skip,
            "未指定 --stun，默认不测（局域网联调不需要公网映射）".to_owned(),
        );
    };
    let slack = timeout + Duration::from_secs(1);
    match tokio::time::timeout(slack, ak_signal::stun::query(server, timeout)).await {
        Ok(Ok((local, mapped))) => CheckResult::new(
            "stun",
            CheckStatus::Ok,
            format!("查询成功：本机 {local}，公网映射地址 {mapped}"),
        ),
        Ok(Err(err)) => CheckResult::new(
            "stun",
            CheckStatus::Warn,
            format!("查询失败（{server}）：{err}"),
        )
        .with_advice("确认 STUN 服务器地址可达且 UDP 出站未被拦截；局域网联调可忽略此项"),
        Err(_) => CheckResult::new(
            "stun",
            CheckStatus::Warn,
            format!("查询超时（{}s 内无响应，{server}）", timeout.as_secs()),
        )
        .with_advice("STUN 服务器不可达或 UDP 出站被拦截；局域网联调可忽略此项"),
    }
}

/// Free space on the volume holding the id directory (where checkpoints and
/// part files land on this machine).
fn probe_disk(id: &Path) -> CheckResult {
    match ak_storage::LocalSaveTarget::open(id) {
        Ok(target) => match target.available_bytes() {
            Some(free) => CheckResult::new(
                "disk",
                CheckStatus::Ok,
                format!("id 目录所在卷可用空间 {}", human_bytes(free)),
            ),
            None => CheckResult::new(
                "disk",
                CheckStatus::Warn,
                "平台无法报告该卷的可用空间".to_owned(),
            )
            .with_advice("传输前请自行确认磁盘余量；空间不足时引擎会以 StorageFull 明确报错"),
        },
        Err(err) => CheckResult::new(
            "disk",
            CheckStatus::Warn,
            format!("无法打开 id 目录查询卷信息：{err}"),
        )
        .with_advice("确认 id 目录存在且可写"),
    }
}

// ---------------------------------------------------------------------------
// Summary and rendering
// ---------------------------------------------------------------------------

/// One-line conclusion plus the standing two-machine advice (firewall first
/// prompt, AP isolation) whenever anything is not ok.
fn conclusion(results: &[CheckResult]) -> String {
    let warned: Vec<&str> = results
        .iter()
        .filter(|result| result.status == CheckStatus::Warn)
        .map(|result| result.name)
        .collect();
    let skipped: Vec<&str> = results
        .iter()
        .filter(|result| result.status == CheckStatus::Skip)
        .map(|result| result.name)
        .collect();
    if warned.is_empty() && skipped.is_empty() {
        return "结论：环境自检全部通过，可开始两机联调（接收端先 `ak-cli receive`，发送端 `ak-cli send ... --to 对端:7100`）。"
            .to_owned();
    }
    let mut parts = Vec::new();
    if !warned.is_empty() {
        parts.push(format!("{} 项告警（{}）", warned.len(), warned.join("/")));
    }
    if !skipped.is_empty() {
        parts.push(format!("{} 项跳过（{}）", skipped.len(), skipped.join("/")));
    }
    format!(
        "结论：{}。若两机互不可达：Windows 首次监听会弹防火墙提示，选择“允许”后仅开放该直连端口；\
         酒店/访客/公司 Wi-Fi 常开 AP 隔离，同网设备也互不可达，此时改用手动 IP（send --to）或公网 probe 流程。",
        parts.join("，")
    )
}

/// Human text report: one status line per check plus advice lines.
fn render_text(results: &[CheckResult], summary: &str) -> String {
    let mut out = String::from("ak-cli doctor: 环境自检（每项 ok/warn/skip，失败只报告不中断）\n");
    for result in results {
        out.push_str(&format!(
            "[{:<4}] {:<10} {}\n",
            result.status.as_str(),
            result.name,
            result.detail
        ));
        for line in &result.extra {
            out.push_str(&format!("                  {line}\n"));
        }
        if let Some(advice) = &result.advice {
            out.push_str(&format!("                  建议：{advice}\n"));
        }
    }
    out.push_str(summary);
    out.push('\n');
    out
}

/// Machine-readable JSON report (same fields as the text form).
fn render_json(results: &[CheckResult], summary: &str) -> String {
    let mut out = String::from("{\n  \"tool\": \"ak-cli doctor\",\n  \"items\": [\n");
    for (index, result) in results.iter().enumerate() {
        let extra = if result.extra.is_empty() {
            "[]".to_owned()
        } else {
            let items: Vec<String> = result
                .extra
                .iter()
                .map(|line| format!("\"{}\"", json_escape(line)))
                .collect();
            format!("[{}]", items.join(", "))
        };
        let advice = match &result.advice {
            Some(advice) => format!("\"{}\"", json_escape(advice)),
            None => "null".to_owned(),
        };
        let comma = if index + 1 < results.len() { "," } else { "" };
        out.push_str(&format!(
            "    {{\"name\": \"{}\", \"status\": \"{}\", \"detail\": \"{}\", \"extra\": {extra}, \"advice\": {advice}}}{comma}\n",
            result.name,
            result.status.as_str(),
            json_escape(&result.detail),
        ));
    }
    out.push_str(&format!(
        "  ],\n  \"summary\": \"{}\"\n}}",
        json_escape(summary)
    ));
    out
}

// ---------------------------------------------------------------------------
// Command entry
// ---------------------------------------------------------------------------

/// Run every probe in order and print the report (text or JSON). Never fails
/// on probe errors; only printing/IO can fail.
pub(crate) async fn doctor_command(
    id: &Path,
    stun: Option<SocketAddr>,
    mdns_seconds: u64,
    json: bool,
) -> Result<()> {
    let mut results = Vec::new();

    let (identity, fingerprint) = probe_identity(id);
    results.push(identity);

    results.push(probe_local_nets());

    results.push(probe_udp_bind().await);

    // mDNS is synchronous inside mdns-sd's daemon threads; run the bounded
    // round on a blocking worker with an outer wall-clock guard.
    let mdns = match fingerprint {
        Some(fingerprint) => {
            let device_name = whoami();
            let worker = tokio::task::spawn_blocking(move || {
                probe_mdns(&fingerprint, &device_name, mdns_seconds)
            });
            match tokio::time::timeout(Duration::from_secs(mdns_seconds) + MDNS_SLACK, worker).await
            {
                Ok(Ok(result)) => result,
                Ok(Err(join)) => CheckResult::new(
                    "mdns",
                    CheckStatus::Warn,
                    format!("mDNS 探测线程失败：{join}"),
                ),
                Err(_) => CheckResult::new(
                    "mdns",
                    CheckStatus::Warn,
                    format!("mDNS 探测超时未返回（>{}s，组播环境异常）", mdns_seconds),
                ),
            }
        }
        None => CheckResult::new(
            "mdns",
            CheckStatus::Skip,
            "设备密钥不可用，跳过 mDNS 探测".to_owned(),
        ),
    };
    results.push(mdns);

    results.push(probe_stun(stun, STUN_TIMEOUT).await);

    results.push(probe_disk(id));

    let summary = conclusion(&results);
    if json {
        println!("{}", render_json(&results, &summary));
    } else {
        print!("{}", render_text(&results, &summary));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ak_identity::{Fingerprint, PeerBook, PeerRecord};

    /// Unique temp directory helper (tests may use expect/unwrap).
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ak-cli-doctor-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn net(addr: &str, prefix_len: u8) -> ak_net::classify::Net {
        ak_net::classify::Net {
            addr: addr.parse().expect("test ip"),
            prefix_len,
        }
    }

    // -- identity ------------------------------------------------------------

    #[test]
    fn identity_probe_reports_created_then_existing_with_peer_count() {
        let dir = TempDir::new("identity");

        // Fresh directory: key created, empty book earns pairing advice.
        let (first, fp) = probe_identity(dir.path());
        assert_eq!(first.status, CheckStatus::Ok);
        assert!(first.detail.contains("新建"), "{}", first.detail);
        assert!(first.detail.contains("0 条"), "{}", first.detail);
        assert!(
            first.detail.contains(&fp.expect("fp").short_id()),
            "{}",
            first.detail
        );
        assert!(first.advice.as_deref().is_some_and(|a| a.contains("pair")));

        // The fingerprint is stable across loads.
        let fp_hex = fp.expect("fp").to_hex();

        // One paired peer: advice gone, count 1, key reported as existing.
        let mut book = PeerBook::default();
        book.upsert(PeerRecord {
            fingerprint: Fingerprint::from_bytes([7u8; 32]),
            name: "laptop".to_owned(),
            paired_unix_ms: 1,
        });
        book.save(&peer_book_path(dir.path())).unwrap();

        let (second, second_fp) = probe_identity(dir.path());
        assert_eq!(second.status, CheckStatus::Ok);
        assert!(second.detail.contains("已存在"), "{}", second.detail);
        assert!(second.detail.contains("1 条"), "{}", second.detail);
        assert!(second.advice.is_none());
        assert!(second.detail.contains(&fp_hex[..8]), "{}", second.detail);
        assert_eq!(second_fp.map(|f| f.to_hex()), Some(fp_hex));
    }

    #[test]
    fn identity_probe_warns_on_unreadable_peer_book() {
        let dir = TempDir::new("identity-badbook");
        let _ = probe_identity(dir.path()); // create device.key
        std::fs::write(peer_book_path(dir.path()), b"not a peer book").unwrap();
        let (result, fp) = probe_identity(dir.path());
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.detail.contains("配对簿不可读"), "{}", result.detail);
        assert!(fp.is_some(), "key still loads");
    }

    // -- local nets ------------------------------------------------------------

    #[test]
    fn format_nets_groups_by_family_keeping_order() {
        let nets = vec![
            net("192.168.1.23", 24),
            net("fe80::a1", 64),
            net("10.0.0.5", 8),
            net("2001:db8::9", 64),
        ];
        let (v4, v6) = format_nets(&nets);
        assert_eq!(v4, "192.168.1.23/24, 10.0.0.5/8");
        assert_eq!(v6, "fe80::a1/64, 2001:db8::9/64");

        let (empty4, empty6) = format_nets(&[]);
        assert_eq!((empty4.as_str(), empty6.as_str()), ("", ""));
    }

    #[test]
    fn local_nets_probe_reports_ok_or_warn_with_detail() {
        // Best effort: the environment decides; the probe must never panic.
        let result = probe_local_nets();
        assert!(
            result.status == CheckStatus::Ok || result.status == CheckStatus::Warn,
            "{result:?}"
        );
        assert!(!result.detail.is_empty());
    }

    // -- udp bind --------------------------------------------------------------

    #[tokio::test]
    async fn udp_bind_probe_reports_both_families_without_panicking() {
        // Best effort: status depends on the machine's stacks.
        let result = probe_udp_bind().await;
        assert!(
            result.status == CheckStatus::Ok || result.status == CheckStatus::Warn,
            "{result:?}"
        );
        if result.status == CheckStatus::Ok {
            assert!(result.detail.contains("0.0.0.0"), "{}", result.detail);
            assert!(result.detail.contains("[::]"), "{}", result.detail);
        } else {
            assert!(result.advice.is_some(), "warn needs advice: {result:?}");
        }
    }

    // -- mdns ------------------------------------------------------------------

    #[test]
    fn mdns_probe_best_effort_always_returns_a_result() {
        // Best effort: multicast may be unavailable here; every degraded
        // outcome must still be a well-formed result with a reason.
        let fp = Fingerprint::from_bytes([0xab; 32]);
        let start = std::time::Instant::now();
        let result = probe_mdns(&fp, "ak-doctor-test", 1);
        assert!(
            matches!(
                result.status,
                CheckStatus::Ok | CheckStatus::Warn | CheckStatus::Skip
            ),
            "{result:?}"
        );
        assert!(!result.detail.is_empty());
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "probe ran unbounded: {:?}",
            start.elapsed()
        );
    }

    // -- stun ------------------------------------------------------------------

    #[tokio::test]
    async fn stun_probe_skips_without_a_server() {
        let result = probe_stun(None, STUN_TIMEOUT).await;
        assert_eq!(result.status, CheckStatus::Skip);
        assert!(result.detail.contains("--stun"), "{}", result.detail);
        assert!(result.advice.is_none(), "a skip needs no advice");
    }

    #[tokio::test]
    async fn stun_probe_warns_on_dead_server_within_timeout() {
        // 127.0.0.1:1 has no STUN listener: refused or timeout, both Warn,
        // both bounded by the 1s budget (+1s slack).
        let dead = "127.0.0.1:1".parse().expect("addr");
        let start = std::time::Instant::now();
        let result = probe_stun(Some(dead), Duration::from_secs(1)).await;
        assert_eq!(result.status, CheckStatus::Warn, "{result:?}");
        assert!(result.detail.contains("127.0.0.1:1"), "{}", result.detail);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "probe exceeded its budget: {:?}",
            start.elapsed()
        );
    }

    // -- disk ------------------------------------------------------------------

    #[test]
    fn disk_probe_reports_free_space_for_a_real_directory() {
        let dir = TempDir::new("disk");
        let result = probe_disk(dir.path());
        // A real temp directory on a real volume: the platform can report it.
        assert_eq!(result.status, CheckStatus::Ok, "{result:?}");
        assert!(result.detail.contains("B"), "{}", result.detail);
        assert!(result.advice.is_none());
    }

    #[test]
    fn disk_probe_warns_when_directory_cannot_be_opened() {
        let dir = TempDir::new("disk-missing");
        let missing = dir.path().join("nope");
        let result = probe_disk(&missing);
        // LocalSaveTarget refuses missing roots (its open contract).
        assert_eq!(result.status, CheckStatus::Warn, "{result:?}");
        assert!(result.detail.contains("无法打开"), "{}", result.detail);
    }

    // -- summary and rendering ---------------------------------------------------

    fn sample_results() -> Vec<CheckResult> {
        vec![
            CheckResult::new("identity", CheckStatus::Ok, "指纹 abc".to_owned()),
            CheckResult::new("mdns", CheckStatus::Skip, "组播不可用".to_owned()),
            CheckResult::new("udp-bind", CheckStatus::Warn, "IPv6 绑定失败".to_owned())
                .with_advice("检查 IPv6"),
        ]
    }

    #[test]
    fn conclusion_all_ok_announces_readiness() {
        let results = vec![CheckResult::new(
            "identity",
            CheckStatus::Ok,
            "x".to_owned(),
        )];
        let text = conclusion(&results);
        assert!(text.contains("全部通过"), "{text}");
        assert!(!text.contains("防火墙"), "no need for warnings: {text}");
    }

    #[test]
    fn conclusion_with_issues_mentions_firewall_and_ap_isolation() {
        // sample_results(): 1 warn (udp-bind) + 1 skip (mdns) + 1 ok.
        let text = conclusion(&sample_results());
        assert!(text.contains("1 项告警（udp-bind）"), "{text}");
        assert!(text.contains("1 项跳过（mdns）"), "{text}");
        assert!(text.contains("防火墙"), "{text}");
        assert!(text.contains("AP 隔离"), "{text}");
        assert!(text.contains("手动 IP"), "{text}");
    }

    #[test]
    fn text_report_lists_every_check_with_status_and_advice() {
        let results = sample_results();
        let text = render_text(&results, "结论：示例。");
        assert!(text.contains("ak-cli doctor"), "{text}");
        assert!(text.contains("[ok  ] identity"), "{text}");
        assert!(text.contains("[skip] mdns"), "{text}");
        assert!(text.contains("[warn] udp-bind"), "{text}");
        assert!(text.contains("建议：检查 IPv6"), "{text}");
        assert!(text.ends_with("结论：示例。\n"), "{text}");
    }

    #[test]
    fn json_report_has_machine_readable_shape() {
        let results = vec![
            CheckResult::new("identity", CheckStatus::Ok, "指纹 \"abc\"".to_owned())
                .with_extra(vec!["v4: 10.0.0.2/8".to_owned()]),
            CheckResult::new("mdns", CheckStatus::Skip, "组播\n不可用".to_owned())
                .with_advice("改用手动 IP"),
            CheckResult::new("stun", CheckStatus::Warn, "超时".to_owned()),
        ];
        let json = render_json(&results, "结论：示例");
        assert!(json.starts_with('{') && json.ends_with('}'), "{json}");
        assert!(json.contains("\"tool\": \"ak-cli doctor\""), "{json}");
        assert!(json.contains("\"name\": \"identity\""), "{json}");
        assert!(json.contains("\"status\": \"ok\""), "{json}");
        assert!(json.contains("\"status\": \"skip\""), "{json}");
        assert!(json.contains("\"status\": \"warn\""), "{json}");
        // Quotes and newlines in details are escaped.
        assert!(json.contains("指纹 \\\"abc\\\""), "{json}");
        assert!(json.contains("组播\\n不可用"), "{json}");
        // Extra lines ride along as an array; missing advice is null.
        assert!(json.contains("\"extra\": [\"v4: 10.0.0.2/8\"]"), "{json}");
        assert!(json.contains("\"advice\": \"改用手动 IP\""), "{json}");
        assert!(json.contains("\"advice\": null"), "{json}");
        assert!(json.contains("\"summary\": \"结论：示例\""), "{json}");
        assert!(!json.contains("TODO") && !json.contains("NaN"), "{json}");
    }
}
