//! `MetaFiltered<S>`: birincil stratejiyi meta modelle süzen sarmalayıcı.
//!
//! - Birincil strateji olduğu gibi çalışır; yalnız ürettiği `Open` niyetleri süzülür.
//! - Her `Open` için özellikler `ctx.now()` anında **kullanılabilir** son satırdan okunur
//!   (`available_at ≤ ctx.now()`, kural 4); eğitimdekiyle aynı fonksiyon ([`PitColumns::row_at`]).
//! - `p = model(yön, özellikler)`; [`Policy`] girme derse niyet atılır, girerse adet
//!   `max(1, ⌊adet · m⌋)` olur. Özellik okunamıyorsa (ısınma, başka enstrüman) niyet atılır.
//! - `Close` ve `CancelEntry` dokunulmadan geçer (riski azaltır).
//! - Kimlik birincil stratejininkidir: niyetler onun kimliğini taşır, motor pozisyonu bu kimlikle izler.

use crate::MlError;
use crate::columns::{PitColumns, last_available};
use crate::decide::Policy;
use crate::model::MetaModel;
use atlas_core::{
    Action, Ctx, InstrumentId, Intent, MarketEvent, MarketPayload, SessionSpec, Strategy,
    StrategyId, UnixNanos,
};
use atlas_features::{FeatureFrame, FeatureSpec};

enum Source {
    /// Backtest: önceden hesaplanmış sütunlar.
    Columns(PitColumns),
    /// Canlı/gölge: barlar geldikçe büyüyen çerçeve; `order[k]` = modelin k. özelliğinin sütunu.
    Live {
        frame: Box<FeatureFrame>,
        order: Vec<usize>,
    },
}

/// Süzme sayaçları (gözlem/rapor için).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetaStats {
    pub opens_seen: u64,
    pub passed: u64,
    /// Adedi küçültülerek geçenler (`passed`'ın alt kümesi).
    pub resized: u64,
    pub dropped_low_p: u64,
    pub dropped_no_features: u64,
}

pub struct MetaFiltered<S: Strategy> {
    inner: S,
    model: MetaModel,
    policy: Policy,
    inst: InstrumentId,
    src: Source,
    stats: MetaStats,
}

fn check_names(model: &MetaModel, names: &[String]) -> Result<(), MlError> {
    if model.names.as_slice() != names {
        return Err(MlError::FeatureMismatch {
            model: model.names.clone(),
            source_names: names.to_vec(),
        });
    }
    Ok(())
}

impl<S: Strategy> MetaFiltered<S> {
    /// Backtest: `inst` enstrümanının önceden hesaplanmış sütunlarıyla. Sütun adları modelinkiyle
    /// aynı sırada olmalı.
    pub fn with_columns(
        inner: S,
        model: MetaModel,
        policy: Policy,
        inst: InstrumentId,
        cols: PitColumns,
    ) -> Result<Self, MlError> {
        check_names(&model, cols.names())?;
        Ok(MetaFiltered {
            inner,
            model,
            policy,
            inst,
            src: Source::Columns(cols),
            stats: MetaStats::default(),
        })
    }

    /// Canlı/gölge: `inst` barları geldikçe feature'ları kendisi hesaplar (aynı hesaplayıcılar).
    pub fn live(
        inner: S,
        model: MetaModel,
        policy: Policy,
        inst: InstrumentId,
        specs: &[FeatureSpec],
        session: SessionSpec,
    ) -> Result<Self, MlError> {
        let names: Vec<String> = specs.iter().map(ToString::to_string).collect();
        check_names(&model, &names)?;
        let frame = FeatureFrame::new(specs, session);
        let order = specs
            .iter()
            .map(|s| {
                frame
                    .index_of(s)
                    .ok_or_else(|| MlError::InvalidInput(format!("{s} çerçevede yok")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(MetaFiltered {
            inner,
            model,
            policy,
            inst,
            src: Source::Live {
                frame: Box::new(frame),
                order,
            },
            stats: MetaStats::default(),
        })
    }

    pub fn stats(&self) -> MetaStats {
        self.stats
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }

    pub fn model(&self) -> &MetaModel {
        &self.model
    }

    pub fn into_inner(self) -> S {
        self.inner
    }

    /// `now` anında kullanılabilir özellik satırı.
    fn row_at(&self, now: UnixNanos) -> Option<Vec<f64>> {
        match &self.src {
            Source::Columns(c) => c.row_at(now),
            Source::Live { frame, order } => {
                let i = last_available(frame.available_at(), now)?;
                order.iter().map(|&c| frame.value_at(c, i, now)).collect()
            }
        }
    }

    fn filter(&mut self, now: UnixNanos, mut it: Intent) -> Option<Intent> {
        let Action::Open { side, qty, .. } = &mut it.action else {
            return Some(it);
        };
        self.stats.opens_seen += 1;
        let p = if it.instrument == self.inst {
            self.row_at(now)
                .and_then(|row| self.model.probability(*side, &row))
        } else {
            None
        };
        let Some(p) = p else {
            self.stats.dropped_no_features += 1;
            return None;
        };
        let d = self.policy.decide(p);
        let Some(q) = Policy::scale_qty(*qty, &d) else {
            self.stats.dropped_low_p += 1;
            return None;
        };
        if q < *qty {
            self.stats.resized += 1;
        }
        *qty = q;
        self.stats.passed += 1;
        Some(it)
    }
}

impl<S: Strategy> Strategy for MetaFiltered<S> {
    fn id(&self) -> &StrategyId {
        self.inner.id()
    }

    fn on_event(&mut self, ctx: &Ctx<'_>, ev: &MarketEvent) -> Vec<Intent> {
        if let (Source::Live { frame, .. }, MarketPayload::Bar(b)) = (&mut self.src, &ev.payload)
            && ev.instrument == self.inst
        {
            frame.push(b);
        }
        let now = ctx.now();
        let intents = self.inner.on_event(ctx, ev);
        intents
            .into_iter()
            .filter_map(|it| self.filter(now, it))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cv::CvReport;
    use crate::dataset::Transform;
    use crate::decide::Sizing;
    use crate::model::{LogisticModel, LogitConfig, MODEL_VERSION, TrainingSummary};
    use atlas_core::{
        AccountView, Bar, EntryType, MarketHistory, Money, PositionView, Price, SessionView, Side,
    };
    use std::sync::Arc;

    struct Acct;
    impl AccountView for Acct {
        fn position(&self, _: &StrategyId, _: InstrumentId) -> Option<PositionView> {
            None
        }
        fn has_pending_entry(&self, _: &StrategyId, _: InstrumentId) -> bool {
            false
        }
        fn equity(&self) -> Money {
            Money::from_usd(100_000)
        }
    }
    struct Sess;
    impl SessionView for Sess {
        fn is_open(&self, _: InstrumentId, _: UnixNanos) -> bool {
            true
        }
        fn time_to_session_close(&self, _: InstrumentId, _: UnixNanos) -> Option<u64> {
            None
        }
        fn time_to_week_close(&self, _: InstrumentId, _: UnixNanos) -> Option<u64> {
            None
        }
    }

    /// Her barda 10 lotluk alış + bir kapanış niyeti üreten birincil strateji.
    struct Always(StrategyId);
    impl Strategy for Always {
        fn id(&self) -> &StrategyId {
            &self.0
        }
        fn on_event(&mut self, ctx: &Ctx<'_>, ev: &MarketEvent) -> Vec<Intent> {
            let mk = |action| Intent {
                strategy: self.0.clone(),
                instrument: ev.instrument,
                ts: ctx.now(),
                action,
            };
            vec![
                mk(Action::Open {
                    side: Side::Buy,
                    qty: 10,
                    entry: EntryType::Market,
                    stop_loss: Price(1),
                    take_profit: None,
                    time_exit: None,
                    entry_expiry: None,
                }),
                mk(Action::Close { reason: "x".into() }),
            ]
        }
    }

    /// p = σ(w·x) — tek özellik, standartlaştırma yok.
    fn model(w: f64) -> MetaModel {
        MetaModel {
            version: MODEL_VERSION,
            names: vec!["f".into()],
            transform: Transform::SideSigned,
            fit: LogitConfig::default(),
            model: LogisticModel {
                mean: vec![0.0],
                std: vec![1.0],
                weights: vec![w],
                bias: 0.0,
                iterations: 0,
                converged: true,
            },
            summary: TrainingSummary {
                n: 0,
                positive_rate: 0.0,
                t_start: UnixNanos::ZERO,
                t_end: UnixNanos::ZERO,
                skipped_no_features: 0,
                cv: CvReport {
                    k: 5,
                    embargo_ns: 0,
                    folds: vec![],
                    auc: None,
                    log_loss: None,
                    mean_fold_auc: None,
                    n_oof: 0,
                },
            },
        }
    }

    fn bar(i: u64) -> Bar {
        let open_ts = UnixNanos(i * 60);
        Bar {
            open_ts,
            close_ts: open_ts.plus(60),
            open: Price(100),
            high: Price(101),
            low: Price(99),
            close: Price(100),
            volume: 1,
            buy_volume: 0,
            trades: 1,
        }
    }

    /// Barları sırayla motor gibi işler (önce geçmiş, sonra strateji).
    fn run<S: Strategy>(s: &mut S, n: u64) -> Vec<Vec<Intent>> {
        let id = s.id().clone();
        let mut h = MarketHistory::new(0);
        (0..n)
            .map(|i| {
                let ev = MarketEvent::bar(InstrumentId(0), bar(i));
                h.push(&ev);
                let ctx = Ctx::new(&id, &h, &Acct, &Sess);
                s.on_event(&ctx, &ev)
            })
            .collect()
    }

    fn open_qty(v: &[Intent]) -> Option<i32> {
        v.iter().find_map(|i| match i.action {
            Action::Open { qty, .. } => Some(qty),
            _ => None,
        })
    }

    #[test]
    fn drops_low_p_passes_and_resizes_high_p() {
        // Bar i'nin özelliği: i < 2 → −3 (p ≈ 0.05), i ≥ 2 → +3 (p ≈ 0.95), bar 4 → +0.5 (p ≈ 0.62).
        let times: Vec<UnixNanos> = (0..5).map(|i| bar(i).close_ts).collect();
        let cols = PitColumns::new(
            vec!["f".into()],
            Arc::new(times),
            vec![Arc::new(vec![-3.0, -3.0, 3.0, 3.0, 0.5])],
        )
        .unwrap();
        let policy = Policy {
            threshold: 0.55,
            sizing: Sizing::Linear,
        };
        let mut m = MetaFiltered::with_columns(
            Always(StrategyId::new("p")),
            model(1.0),
            policy,
            InstrumentId(0),
            cols,
        )
        .unwrap();
        let out = run(&mut m, 5);
        // Düşük p: Open atılır, Close geçer.
        assert_eq!(open_qty(&out[0]), None);
        assert_eq!(out[0].len(), 1);
        assert!(matches!(out[0][0].action, Action::Close { .. }));
        assert_eq!(open_qty(&out[1]), None);
        // Yüksek p: σ(3) = 0.9526 → m = 0.905 → ⌊9.05⌋ = 9.
        assert_eq!(open_qty(&out[2]), Some(9));
        // σ(0.5) = 0.6225 → m = 0.245 → ⌊2.45⌋ = 2.
        assert_eq!(open_qty(&out[4]), Some(2));
        let st = m.stats();
        assert_eq!(st.opens_seen, 5);
        assert_eq!(st.dropped_low_p, 2);
        assert_eq!(st.passed, 3);
        assert_eq!(st.resized, 3);
        assert_eq!(m.id().as_str(), "p");
    }

    #[test]
    fn future_feature_is_never_used() {
        // Özellik değeri bar kapanışından bir dakika sonra kullanılabilir: ilk barda okunacak satır yok,
        // sonraki barlarda bir önceki satır okunur.
        let times: Vec<UnixNanos> = (0..3).map(|i| bar(i).close_ts.plus(60)).collect();
        let cols = PitColumns::new(
            vec!["f".into()],
            Arc::new(times),
            vec![Arc::new(vec![-3.0, 3.0, 3.0])],
        )
        .unwrap();
        let mut m = MetaFiltered::with_columns(
            Always(StrategyId::new("p")),
            model(1.0),
            Policy::default(),
            InstrumentId(0),
            cols,
        )
        .unwrap();
        let out = run(&mut m, 3);
        assert_eq!(open_qty(&out[0]), None);
        assert_eq!(m.stats().dropped_no_features, 1);
        // Bar 1 anında satır 0 (−3) kullanılabilir → düşük p.
        assert_eq!(open_qty(&out[1]), None);
        assert_eq!(m.stats().dropped_low_p, 1);
        // Bar 2 anında satır 1 (+3).
        assert_eq!(open_qty(&out[2]), Some(9));
    }

    #[test]
    fn rejects_mismatched_names_and_live_mode_works() {
        let cols = PitColumns::new(
            vec!["g".into()],
            Arc::new(vec![UnixNanos(1)]),
            vec![Arc::new(vec![0.0])],
        )
        .unwrap();
        assert!(matches!(
            MetaFiltered::with_columns(
                Always(StrategyId::new("p")),
                model(1.0),
                Policy::default(),
                InstrumentId(0),
                cols,
            ),
            Err(MlError::FeatureMismatch { .. })
        ));
        // Canlı: "close" özelliği; modelin adı da "close" olmalı.
        let mut mm = model(1.0);
        mm.names = vec!["close".into()];
        mm.model.mean = vec![100.0];
        let spec: FeatureSpec = "close".parse().unwrap();
        let mut m = MetaFiltered::live(
            Always(StrategyId::new("p")),
            mm,
            Policy {
                threshold: 0.4,
                sizing: Sizing::Fixed,
            },
            InstrumentId(0),
            &[spec],
            SessionSpec::Always,
        )
        .unwrap();
        // close = 100 = ortalama → p = 0.5 ≥ 0.4 → sabit boyut, 10 lot.
        let out = run(&mut m, 2);
        assert_eq!(open_qty(&out[0]), Some(10));
        assert_eq!(open_qty(&out[1]), Some(10));
    }
}
