//! `atlas-dashboard` — yerel web paneli.
//!
//! Örnek: `atlas-dashboard --home /srv/atlas` → http://127.0.0.1:8787
//! VPS'te: `ssh -L 8787:127.0.0.1:8787 vps` ile tünel açıp yerel tarayıcıdan bağlanın.

use anyhow::{Context, Result, bail};
use atlas_dashboard::{AppState, CliConfig, loopback_hosts, router};
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "atlas-dashboard",
    version,
    about = "Atlas web paneli (salt-okunur; veri `atlas --json` alt sürecinden)"
)]
struct Args {
    /// Dinleme adresi. Varsayılan yalnız localhost.
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,
    /// Localhost dışı bir adreste dinlemeye izin ver. Kimlik doğrulama YOK — önerilmez; SSH tüneli kullanın.
    #[arg(long)]
    allow_remote: bool,
    /// Ek kabul edilen Host başlığı (ör. ters vekil arkasında `panel.local:8787`). Tekrarlanabilir.
    #[arg(long = "allow-host")]
    allow_host: Vec<String>,
    /// `atlas` yürütülebilir dosyası. Varsayılan: $ATLAS_BIN, yoksa bu ikilinin yanındaki `atlas`, yoksa PATH.
    #[arg(long)]
    atlas_bin: Option<PathBuf>,
    /// ATLAS_HOME (configs/ içeren kök); alt sürece iletilir. Varsayılan: $ATLAS_HOME.
    #[arg(long)]
    home: Option<PathBuf>,
    /// CLI komutu başına zaman aşımı (sn).
    #[arg(long, default_value_t = 30)]
    timeout_secs: u64,
}

fn default_atlas_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("ATLAS_BIN") {
        return PathBuf::from(p);
    }
    let name = format!("atlas{}", std::env::consts::EXE_SUFFIX);
    if let Ok(me) = std::env::current_exe()
        && let Some(dir) = me.parent()
    {
        let sibling = dir.join(&name);
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from(name)
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = Args::parse();
    if !a.listen.ip().is_loopback() && !a.allow_remote {
        bail!(
            "{} localhost değil. Panelde kimlik doğrulama yok; SSH tüneli kullanın ya da bilerek --allow-remote verin",
            a.listen
        );
    }
    let home = a
        .home
        .or_else(|| std::env::var_os("ATLAS_HOME").map(PathBuf::from));
    if let Some(h) = &home
        && !h.join("configs").join("atlas.toml").is_file()
    {
        bail!(
            "{}: configs/atlas.toml yok (ATLAS_HOME doğru mu?)",
            h.display()
        );
    }
    let mut cli = CliConfig::new(a.atlas_bin.unwrap_or_else(default_atlas_bin), home);
    cli.timeout = Duration::from_secs(a.timeout_secs.max(1));
    cli.check_bin().map_err(anyhow::Error::msg)?;

    let mut hosts = loopback_hosts(a.listen.port());
    hosts.push(a.listen.to_string());
    hosts.extend(a.allow_host);
    let state = Arc::new(AppState::new(cli, hosts));

    let listener = tokio::net::TcpListener::bind(a.listen)
        .await
        .with_context(|| format!("{} dinlenemedi", a.listen))?;
    eprintln!(
        "Atlas paneli: http://{} (atlas: {}, ATLAS_HOME: {})",
        a.listen,
        state.cli.atlas_bin.display(),
        state
            .cli
            .atlas_home
            .as_ref()
            .map_or("miras".to_string(), |h| h.display().to_string())
    );
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("sunucu hatası")?;
    Ok(())
}
