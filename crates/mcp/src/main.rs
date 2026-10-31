//! `atlas-mcp` — Grok Bot için uzak MCP sunucusu (ADR 0009).
//!
//! Üretim (systemd `atlas-mcp.service`, kullanıcı `grok`, nginx TLS arkasında):
//!   ATLAS_MCP_TOKEN=… atlas-mcp --listen 127.0.0.1:8790 \
//!     --exec sudo --exec-prefix=-n --exec-prefix=-u --exec-prefix=atlas --exec-prefix=/usr/local/lib/atlas/atlas-grok
//! Yerel deneme: ATLAS_MCP_TOKEN=… atlas-mcp --exec target/release/atlas --home .

use anyhow::{Result, bail};
use atlas_dashboard::CliConfig;
use atlas_mcp::{McpState, router};
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "atlas-mcp",
    version,
    about = "Atlas MCP sunucusu (Grok operatör araçları)"
)]
struct Args {
    /// Dinleme adresi. Yalnız localhost (TLS ve dış erişim nginx'te).
    #[arg(long, default_value = "127.0.0.1:8790")]
    listen: SocketAddr,
    /// Çalıştırılacak program: `atlas` ikilisi ya da `sudo`.
    #[arg(long)]
    exec: PathBuf,
    /// Komuttan önce eklenecek argümanlar (tekrarlanabilir), ör. sudo için `-n -u atlas <yardımcı>`.
    #[arg(long = "exec-prefix", allow_hyphen_values = true)]
    exec_prefix: Vec<String>,
    /// ATLAS_HOME (yerel deneme). Üretimde yardımcı kendisi sabitler.
    #[arg(long)]
    home: Option<PathBuf>,
    /// Normal araç zaman aşımı (sn).
    #[arg(long, default_value_t = 60)]
    timeout_secs: u64,
    /// verify zaman aşımı (sn).
    #[arg(long, default_value_t = 900)]
    slow_timeout_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = Args::parse();
    if !a.listen.ip().is_loopback() {
        bail!(
            "{}: yalnız localhost; dış erişim nginx (TLS) üzerinden",
            a.listen
        );
    }
    let token = std::env::var("ATLAS_MCP_TOKEN").unwrap_or_default();
    let mut cli = CliConfig::new(a.exec, a.home);
    cli.prefix = a.exec_prefix;
    cli.timeout = Duration::from_secs(a.timeout_secs.max(1));
    cli.check_bin().map_err(anyhow::Error::msg)?;
    let mut st = McpState::new(cli, &token).map_err(anyhow::Error::msg)?;
    st.slow_timeout = Duration::from_secs(a.slow_timeout_secs.max(1));
    let app = router(Arc::new(st));
    let listener = tokio::net::TcpListener::bind(a.listen).await?;
    eprintln!("atlas-mcp dinliyor: http://{}/mcp", a.listen);
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
