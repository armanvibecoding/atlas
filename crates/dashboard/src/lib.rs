//! Atlas dashboard (§3, M4): Axum web paneli + JSON API.
//!
//! Tek veri kaynağı `atlas` CLI'dır: her API isteği beyaz listedeki salt-okunur bir komutu
//! `atlas --json …` alt süreciyle çalıştırır ve `atlas.cli/1` zarfını aynen döndürür. Böylece panel
//! CLI'nın gösterdiği her şeyi, aynı izin kontrolünden (`ATLAS_ROLE=grok`) geçerek gösterir.
//! Gerekçe ve güvenlik modeli: `docs/adr/0007-dashboard.md`.
//!
//! Güvenlik özeti:
//! - Yalnız `GET`; yazma/eylem komutu (kill, note, approve, gauntlet, data pull…) beyaz listede yok.
//! - Argümanlar sıkı doğrulanır, kabuk kullanılmaz.
//! - Varsayılan dinleme `127.0.0.1:8787`; kimlik doğrulama yok, bu yüzden dışarı açmak açık bayrak
//!   ister. VPS'te SSH tüneliyle erişilir. `Host` başlığı denetlenir (DNS rebinding).

pub mod commands;
pub mod runner;
pub mod server;

pub use commands::{EXCLUDED, READ_COMMANDS};
pub use runner::{CliConfig, CliOutcome};
pub use server::{AppState, loopback_hosts, router};
