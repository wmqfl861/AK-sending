//! Command-line prototype for AK-sending (single binary, many verbs).
//!
//! Verbs: `keygen`, `fingerprint`, `receive`, `send`, `signal-server`,
//! `probe`, `discover`. The binary never talks to any file-carrying server;
//! servers started here are coordination-only.
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "ak-cli", version, about = "AK-sending direct-only prototype")]
pub struct Cli {
    /// Identity directory (default: <home>/.ak-sending).
    #[arg(long, global = true)]
    pub id_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create or show the device key, fingerprint, and pairing code.
    Keygen,
    /// Print the current fingerprint and pairing code.
    Fingerprint,
    /// Receive one transfer into a directory.
    Receive {
        /// Destination directory (must exist).
        #[arg(long, default_value = ".")]
        out: PathBuf,
        /// UDP port to listen on.
        #[arg(long, default_value_t = 7100)]
        port: u16,
        /// Only accept this peer fingerprint (64 hex or pairing code).
        #[arg(long)]
        expect: Option<String>,
        /// Overwrite policy: error (default) or rename.
        #[arg(long, default_value = "error")]
        conflict: String,
    },
    /// Send files or a directory to a paired peer.
    Send {
        /// Files or one directory to send.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Peer address ip:port.
        #[arg(long)]
        to: SocketAddr,
        /// Peer fingerprint (64 hex or pairing code), pinned for this transfer.
        #[arg(long)]
        fp: String,
        /// Local UDP port (default auto).
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[arg(long, default_value = "error")]
        conflict: String,
    },
    /// Discover peers on the local network via mDNS (bounded browse).
    Discover {
        #[arg(long, default_value_t = 3)]
        seconds: u64,
    },
    /// Run the coordination server (signaling + STUN; no file routes).
    SignalServer {
        #[arg(long, default_value_t = 7100)]
        tcp_port: u16,
        #[arg(long, default_value_t = 7101)]
        stun_port: u16,
    },
    /// Public direct-connect probe: register, exchange candidates, punch, QUIC.
    Probe {
        #[arg(long)]
        server: SocketAddr,
        /// Peer device id (fingerprint hex) to punch toward.
        #[arg(long)]
        peer: String,
        /// Skip STUN mapping discovery.
        #[arg(long, default_value_t = false)]
        no_stun: bool,
    },
}

fn main() -> anyhow::Result<()> {
    todo!("owned by integration phase")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "replaced during integration"]
    fn placeholder() {}
}
