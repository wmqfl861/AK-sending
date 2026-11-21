//! Argument shapes and parsers: `--bind` values, listen addresses, shell
//! completions and the identity directory.
//!
//! Moved out of `main.rs` unchanged (CR-P2-08, batch U review).

use super::*;

// ---------------------------------------------------------------------------
// --bind value shapes (receive / signal-server listen overrides)
// ---------------------------------------------------------------------------

/// Parsed form of a `--bind` / `--stun-bind` value: either a bare IP whose
/// port comes from the verb's port flag, or a full socket address carrying
/// its own port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BindArg {
    /// Bare IPv4/IPv6 address (`0.0.0.0`, `::`, `192.168.1.5`).
    Ip(IpAddr),
    /// Full ip:port / `[v6]:port` (`0.0.0.0:7100`, `[::]:7101`).
    Addr(SocketAddr),
}

impl BindArg {
    /// Accept a full socket address first (`[::]:7100`, `1.2.3.4:5`), then a
    /// bare IP (`::`, `0.0.0.0`); anything else is an explicit error naming
    /// the expected shapes. Whitespace is trimmed like every other value.
    pub(super) fn parse(text: &str) -> Result<Self> {
        let trimmed = text.trim();
        if let Ok(addr) = trimmed.parse::<SocketAddr>() {
            return Ok(BindArg::Addr(addr));
        }
        if let Ok(ip) = trimmed.parse::<IpAddr>() {
            return Ok(BindArg::Ip(ip));
        }
        bail!(
            "invalid bind address {trimmed:?}: expected a bare IP (`::`, `0.0.0.0`, \
             `192.168.1.5`) or ip:port (`[::]:7100`, `0.0.0.0:7100`)"
        )
    }

    /// The final listen address, combining a bare IP with `fallback_port`.
    fn into_addr(self, fallback_port: u16) -> SocketAddr {
        match self {
            BindArg::Ip(ip) => SocketAddr::new(ip, fallback_port),
            BindArg::Addr(addr) => addr,
        }
    }
}

/// clap value parser bridging [`BindArg::parse`] errors into help output.
pub(super) fn bind_arg_parser(text: &str) -> Result<BindArg, String> {
    BindArg::parse(text).map_err(|err| err.to_string())
}

/// clap value parser for one `--to` address item (the flag itself splits on
/// commas via `value_delimiter`). Whitespace around an item is trimmed so
/// `--to "1.2.3.4:7000, 5.6.7.8:7000"` parses; anything that is not a full
/// socket address is an explicit error naming the expected shapes.
pub(super) fn to_addr_parser(text: &str) -> Result<SocketAddr, String> {
    let trimmed = text.trim();
    trimmed.parse::<SocketAddr>().map_err(|_| {
        format!(
            "invalid peer address {trimmed:?}: expected ip:port (`1.2.3.4:7100`) or \
             [v6]:port (`[2001:db8::1]:7100`); comma-separate multiple candidates"
        )
    })
}

/// Combine a bind flag with the (optional, explicitly given) port flag and
/// the verb's default port into the final listen address. When the bind
/// value carries its own port AND a different explicit port flag was given,
/// the bind port wins and a notice naming the ignored flag is returned;
/// bare-IP binds combine with the port flag by design (no notice).
pub(super) fn resolve_listen_addr(
    bind: Option<BindArg>,
    bind_flag: &str,
    port_flag: Option<u16>,
    port_flag_name: &str,
    default_port: u16,
) -> (SocketAddr, Option<String>) {
    match bind {
        None => (
            SocketAddr::from(([0, 0, 0, 0], port_flag.unwrap_or(default_port))),
            None,
        ),
        Some(arg @ BindArg::Ip(_)) => (arg.into_addr(port_flag.unwrap_or(default_port)), None),
        Some(BindArg::Addr(addr)) => {
            let note = match port_flag {
                Some(explicit) if explicit != addr.port() => Some(format!(
                    "note: {bind_flag} includes its own port {}; ignoring \
                     {port_flag_name} {explicit}",
                    addr.port()
                )),
                _ => None,
            };
            (addr, note)
        }
    }
}

// ---------------------------------------------------------------------------
// completions (clap_complete; the four shells users asked for)
// ---------------------------------------------------------------------------

/// The shells `ak-cli completions` generates (clap_complete also speaks
/// elvish, which nobody requested; keeping our own enum pins the CLI
/// surface independently of the generator's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(super) enum ShellKind {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

impl ShellKind {
    /// Map onto the generator's shell enum (same names, one type boundary).
    fn generator_shell(self) -> clap_complete::aot::Shell {
        match self {
            ShellKind::Bash => clap_complete::aot::Shell::Bash,
            ShellKind::Zsh => clap_complete::aot::Shell::Zsh,
            ShellKind::Fish => clap_complete::aot::Shell::Fish,
            ShellKind::Powershell => clap_complete::aot::Shell::PowerShell,
        }
    }
}

/// Render one shell's completion script into a string (shared by the
/// command and the tests; generation writes nothing on its own).
pub(super) fn completions_script(shell: ShellKind) -> Result<String> {
    let mut cmd = Cli::command();
    let mut buf: Vec<u8> = Vec::new();
    clap_complete::aot::generate(shell.generator_shell(), &mut cmd, "ak-cli", &mut buf);
    String::from_utf8(buf).context("completion script must be UTF-8")
}

/// `completions <shell>`: print the script so shells can consume the pipe.
pub(super) fn completions_command(shell: ShellKind) -> Result<()> {
    let script = completions_script(shell)?;
    print!("{script}");
    let _ = std::io::stdout().flush();
    Ok(())
}

pub(super) fn id_dir(cli: &Cli) -> PathBuf {
    let base = id_dir_path(cli);
    std::fs::create_dir_all(&base).ok();
    base
}
