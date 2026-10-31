//! WASM strateji eklentileri (§4.3 "Plugin", ADR 0004).
//!
//! DSL'in (genom) ifade edemediği fikirler için LLM'in yazdığı Rust, `wasm32-unknown-unknown`'a derlenir
//! ve burada `wasmtime` içinde çalışır. Sandbox kuralları:
//!
//! - **Hiçbir import sağlanmaz.** Ağ, dosya, saat, rastgelelik, WASI yok. Modül tek bir import bile
//!   isterse yüklenmez (`PluginError::ForbiddenImport`); statik denetim atlansa bile boş `Linker`
//!   instantiate'i reddeder.
//! - **Yakıt limiti:** her bar olayı için sabit yakıt; biterse çağrı tuzakla (trap) kesilir, asılma yok.
//! - **Bellek limiti:** `StoreLimits`; tek bellek, sınırı aşan büyüme tuzağa düşer.
//! - **Determinizm:** NaN canonicalization açık, epoch/zaman kesmesi yok, relaxed-SIMD ve thread kapalı.
//!   Aynı girdi dizisi → aynı çıktı dizisi (ve aynı yakıt tüketimi).
//!
//! ## ABI v1
//!
//! Modül şunları dışa aktarır:
//!
//! | Export | İmza | Anlam |
//! |---|---|---|
//! | `memory` | bellek | tek doğrusal bellek |
//! | `alloc` | `(len: i32) -> i32` | `len` baytlık girdi tamponu ayırır, işaretçisini döndürür |
//! | `on_event` | `(ptr: i32, len: i32) -> i64` | girdi JSON'unu okur; çıktıyı `(ptr << 32) \| len` olarak döndürür |
//!
//! Akış (her bar olayında): host `alloc(len)` çağırır, girdi JSON'unu ([`PluginInput`]) dönen adrese yazar,
//! `on_event(ptr, len)` çağırır, dönen değerin yüksek 32 biti çıktı işaretçisi, düşük 32 biti uzunluğudur.
//! Uzunluk 0 = intent yok. Çıktı JSON intent listesidir ([`PluginIntent`]); host onu hemen kopyalar, bu
//! yüzden plugin tamponları bir sonraki çağrıya kadar geçerli tutması yeterlidir.
//!
//! Çıktı **güvenilmez girdidir** (kural 9): şemaya göre (bilinmeyen alan yok) parse edilir ve doğrulanır;
//! geçersizse tamamı atılır ve sayaç artar. `Intent`'i host kurar: strateji kimliği, enstrüman ve zaman
//! damgası her zaman host'tan gelir.

mod abi;
mod host;
mod strategy;

pub use abi::{
    ABI_VERSION, BarInput, OutputError, PluginInput, PluginIntent, PositionInput, parse_output,
};
pub use host::{CallError, PluginModule, PluginRunner};
pub use strategy::{PluginStats, PluginStrategy};

/// Sandbox ve çıktı doğrulama limitleri. Varsayılanlar `Default`'ta; yalnız sıkılaştırılması önerilir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginLimits {
    /// Bir bar olayında (`alloc` + `on_event`) harcanabilecek yakıt (≈ wasm komutu).
    pub fuel_per_event: u64,
    /// Doğrusal belleğin bayt üst sınırı.
    pub max_memory_bytes: usize,
    /// Tablo eleman üst sınırı (dolaylı çağrı tablosu).
    pub max_table_elements: usize,
    /// Plugin'e verilen son bar sayısı.
    pub window_bars: usize,
    /// Çıktı JSON'unun bayt üst sınırı.
    pub max_output_bytes: usize,
    /// Bir olayda dönebilecek intent sayısı üst sınırı.
    pub max_intents: usize,
    /// `open` intent'inde izin verilen en büyük adet.
    pub max_qty: i32,
    /// `time_exit_bars` üst sınırı.
    pub max_time_exit_bars: u32,
    /// Toplam hata (yakıt, tuzak, ABI, geçersiz çıktı) bu sayıya ulaşınca plugin devre dışı kalır.
    pub max_failures: u64,
}

impl Default for PluginLimits {
    fn default() -> Self {
        PluginLimits {
            fuel_per_event: 50_000_000,
            max_memory_bytes: 32 << 20,
            max_table_elements: 10_000,
            window_bars: 256,
            max_output_bytes: 64 << 10,
            max_intents: 8,
            max_qty: 1_000,
            max_time_exit_bars: 100_000,
            max_failures: 10,
        }
    }
}

/// Yükleme/instantiate hataları.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("sandbox motoru kurulamadı: {0}")]
    Engine(String),
    #[error("modül derlenemedi: {0}")]
    Compile(String),
    #[error("yasak import: {module}::{name} (plugin'e hiçbir import sağlanmaz)")]
    ForbiddenImport { module: String, name: String },
    #[error("modül instantiate edilemedi: {0}")]
    Instantiate(String),
    #[error("ABI ihlali: {0}")]
    Abi(String),
    #[error("dosya okunamadı: {0}")]
    Io(String),
}
