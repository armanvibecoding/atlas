//! Meta-labeling (§4.3 "ML"). Kararlar: `docs/adr/0005-ml-meta-labeling.md`.
//!
//! Akış:
//! 1. Birincil sinyal (genom/plugin) aday giriş noktaları üretir.
//! 2. [`label::triple_barrier`] her adayı geleceğe bakarak etiketler (+1 hedef, −1 stop, 0 zaman).
//!    Etiket yalnız eğitimde kullanılır.
//! 3. [`dataset::build_dataset`] her adayın özelliklerini **giriş anına kadar** kullanılabilir
//!    veriden okur ([`columns::PitColumns::row_at`], `available_at ≤ giriş`).
//! 4. [`model::train`] L2 düzenlileştirilmiş lojistik regresyonu (Newton/IRLS) eğitir, [`cv`] ile
//!    purged + embargo'lu k-fold doğrulama yapar ve sonucu modelin özetine yazar.
//! 5. [`decide::Policy`] olasılığı "gir/girme + boyut çarpanı"na çevirir.
//! 6. [`filter::MetaFiltered`] birincil stratejiyi sarar; `Open` niyetlerini süzer ve ölçekler.
//!
//! Harici ML kütüphanesi yok; tüm hesap deterministik (rastgelelik yok, sabit toplama sırası).

pub mod columns;
pub mod cv;
pub mod dataset;
pub mod decide;
pub mod filter;
pub mod label;
pub mod metrics;
pub mod model;

pub use columns::PitColumns;
pub use cv::{CvConfig, CvReport, FoldMetrics, Split, cross_validate, purged_kfold_splits};
pub use dataset::{Dataset, Sample, Transform, build_dataset};
pub use decide::{Decision, Policy, Sizing};
pub use filter::{MetaFiltered, MetaStats};
pub use label::{Barrier, Candidate, Label, Outcome, label_all, triple_barrier};
pub use metrics::{auc, log_loss};
pub use model::{LogisticModel, LogitConfig, MODEL_VERSION, MetaModel, TrainingSummary, train};

/// `ml` crate'inin hata tipi.
#[derive(Debug, thiserror::Error)]
pub enum MlError {
    #[error("geçersiz bariyer: {0}")]
    InvalidBarrier(String),
    #[error("geçersiz girdi: {0}")]
    InvalidInput(String),
    #[error("eğitim verisinde tek sınıf var (pozitif ya da negatif örnek yok)")]
    SingleClass,
    #[error("Hessian pozitif tanımlı değil (tekil sistem)")]
    Singular,
    #[error("model sürümü uyumsuz: dosya {found}, beklenen {expected}")]
    Version { found: u32, expected: u32 },
    #[error("özellik adları modelle uyuşmuyor: model {model:?}, kaynak {source_names:?}")]
    FeatureMismatch {
        model: Vec<String>,
        source_names: Vec<String>,
    },
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
}
