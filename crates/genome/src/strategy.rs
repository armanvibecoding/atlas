//! Genom → derlenmiş `Strategy`.
//!
//! Feature değerleri ya önceden hesaplanmış paylaşılan sütunlardan (backtest, `FeatureStore`) ya da
//! barlar geldikçe büyüyen `FeatureFrame`'den (canlı) okunur. İkisi de aynı hesaplayıcılarla üretilir.
//! Her okuma zaman noktalıdır: değerin `available_at` anı `ctx.now()`'dan sonra ise kullanılmaz.

use crate::{Arith, Cond, FeatureRef, Genome, GenomeError, Operand, parse_ref};
use atlas_core::units::{NANOS_PER_DAY, NANOS_PER_HOUR, NANOS_PER_MIN};
use atlas_core::{
    Action, Ctx, EntryType, InstrumentId, Intent, MarketEvent, MarketPayload, Money, Price,
    SessionSpec, Side, Strategy, StrategyId, UnixNanos,
};
use atlas_features::{FeatureFrame, FeatureSpec, SharedColumns};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Derlenmiş operand: feature sütun indeksi + gecikme ya da sabit.
#[derive(Debug, Clone)]
enum COp {
    Const(f64),
    Feat { col: usize, lag: usize },
    Add(Box<COp>, Box<COp>),
    Sub(Box<COp>, Box<COp>),
    Mul(Box<COp>, Box<COp>),
}

#[derive(Debug, Clone)]
enum CCond {
    All(Vec<CCond>),
    Any(Vec<CCond>),
    Not(Box<CCond>),
    Gt(COp, COp),
    Lt(COp, COp),
    CrossAbove(COp, COp),
    CrossBelow(COp, COp),
}

enum Source {
    Shared {
        times: Arc<Vec<UnixNanos>>,
        cols: Vec<Arc<Vec<f64>>>,
        cursor: usize,
    },
    Live {
        frame: Box<FeatureFrame>,
        order: Vec<usize>,
    },
}

pub struct GenomeStrategy {
    id: StrategyId,
    inst: InstrumentId,
    tick_value: Money,
    genome: Genome,
    long: Option<CCond>,
    short: Option<CCond>,
    atr_col: usize,
    src: Source,
    trades_today: (u64, u32),
}

fn compile_op(o: &Operand, cols: &BTreeMap<FeatureSpec, usize>) -> Result<COp, GenomeError> {
    Ok(match o {
        Operand::Int(i) => COp::Const(*i as f64),
        Operand::Num(x) => COp::Const(*x),
        Operand::Ref(s) => {
            let r = parse_ref(s)?;
            COp::Feat {
                col: *cols
                    .get(&r.spec)
                    .ok_or_else(|| GenomeError::Invalid(format!("{s}: sütun yok")))?,
                lag: r.lag as usize,
            }
        }
        Operand::Op(Arith::Add(p)) => COp::Add(
            Box::new(compile_op(&p.0, cols)?),
            Box::new(compile_op(&p.1, cols)?),
        ),
        Operand::Op(Arith::Sub(p)) => COp::Sub(
            Box::new(compile_op(&p.0, cols)?),
            Box::new(compile_op(&p.1, cols)?),
        ),
        Operand::Op(Arith::Mul(p)) => COp::Mul(
            Box::new(compile_op(&p.0, cols)?),
            Box::new(compile_op(&p.1, cols)?),
        ),
    })
}

fn compile_cond(c: &Cond, cols: &BTreeMap<FeatureSpec, usize>) -> Result<CCond, GenomeError> {
    let pair = |p: &(Operand, Operand)| -> Result<(COp, COp), GenomeError> {
        Ok((compile_op(&p.0, cols)?, compile_op(&p.1, cols)?))
    };
    Ok(match c {
        Cond::All(v) => CCond::All(
            v.iter()
                .map(|x| compile_cond(x, cols))
                .collect::<Result<_, _>>()?,
        ),
        Cond::Any(v) => CCond::Any(
            v.iter()
                .map(|x| compile_cond(x, cols))
                .collect::<Result<_, _>>()?,
        ),
        Cond::Not(x) => CCond::Not(Box::new(compile_cond(x, cols)?)),
        Cond::Gt(p) => {
            let (a, b) = pair(p)?;
            CCond::Gt(a, b)
        }
        Cond::Lt(p) => {
            let (a, b) = pair(p)?;
            CCond::Lt(a, b)
        }
        Cond::CrossAbove(p) => {
            let (a, b) = pair(p)?;
            CCond::CrossAbove(a, b)
        }
        Cond::CrossBelow(p) => {
            let (a, b) = pair(p)?;
            CCond::CrossBelow(a, b)
        }
    })
}

/// Değer okuyucu: `i` = mevcut barın sütun indeksi.
trait Values {
    fn get(&self, col: usize, i: usize) -> Option<f64>;
}

struct SharedView<'a> {
    cols: &'a [Arc<Vec<f64>>],
}
impl Values for SharedView<'_> {
    fn get(&self, col: usize, i: usize) -> Option<f64> {
        let v = *self.cols.get(col)?.get(i)?;
        (!v.is_nan()).then_some(v)
    }
}

struct LiveView<'a> {
    frame: &'a FeatureFrame,
    order: &'a [usize],
}
impl Values for LiveView<'_> {
    fn get(&self, col: usize, i: usize) -> Option<f64> {
        let fc = *self.order.get(col)?;
        let at = *self.frame.available_at().get(i)?;
        self.frame.value_at(fc, i, at)
    }
}

fn eval_op<V: Values + ?Sized>(o: &COp, v: &V, i: usize) -> Option<f64> {
    match o {
        COp::Const(x) => Some(*x),
        COp::Feat { col, lag } => v.get(*col, i.checked_sub(*lag)?),
        COp::Add(a, b) => Some(eval_op(a, v, i)? + eval_op(b, v, i)?),
        COp::Sub(a, b) => Some(eval_op(a, v, i)? - eval_op(b, v, i)?),
        COp::Mul(a, b) => Some(eval_op(a, v, i)? * eval_op(b, v, i)?),
    }
}

fn eval_cond<V: Values + ?Sized>(c: &CCond, v: &V, i: usize) -> bool {
    match c {
        CCond::All(xs) => xs.iter().all(|x| eval_cond(x, v, i)),
        CCond::Any(xs) => xs.iter().any(|x| eval_cond(x, v, i)),
        CCond::Not(x) => !eval_cond(x, v, i),
        CCond::Gt(a, b) => {
            matches!((eval_op(a, v, i), eval_op(b, v, i)), (Some(x), Some(y)) if x > y)
        }
        CCond::Lt(a, b) => {
            matches!((eval_op(a, v, i), eval_op(b, v, i)), (Some(x), Some(y)) if x < y)
        }
        CCond::CrossAbove(a, b) | CCond::CrossBelow(a, b) => {
            let Some(p) = i.checked_sub(1) else {
                return false;
            };
            let vals = (
                eval_op(a, v, i),
                eval_op(b, v, i),
                eval_op(a, v, p),
                eval_op(b, v, p),
            );
            match (c, vals) {
                (CCond::CrossAbove(..), (Some(x), Some(y), Some(px), Some(py))) => {
                    x > y && px <= py
                }
                (CCond::CrossBelow(..), (Some(x), Some(y), Some(px), Some(py))) => {
                    x < y && px >= py
                }
                _ => false,
            }
        }
    }
}

impl GenomeStrategy {
    fn columns_of(
        genome: &Genome,
    ) -> Result<(Vec<FeatureSpec>, BTreeMap<FeatureSpec, usize>), GenomeError> {
        let specs = genome.feature_specs()?;
        let map = specs.iter().enumerate().map(|(i, s)| (*s, i)).collect();
        Ok((specs, map))
    }

    fn build(
        genome: &Genome,
        inst: InstrumentId,
        tick_value: Money,
        map: &BTreeMap<FeatureSpec, usize>,
        src: Source,
    ) -> Result<Self, GenomeError> {
        genome.validate()?;
        let atr_spec = FeatureSpec::new(atlas_features::Kind::Atr, genome.exit.atr);
        Ok(GenomeStrategy {
            id: StrategyId::new(format!("{}#{}", genome.name, &genome.hash()[..10])),
            inst,
            tick_value,
            long: genome
                .entry
                .long
                .as_ref()
                .map(|c| compile_cond(c, map))
                .transpose()?,
            short: genome
                .entry
                .short
                .as_ref()
                .map(|c| compile_cond(c, map))
                .transpose()?,
            atr_col: *map
                .get(&atr_spec)
                .ok_or_else(|| GenomeError::Invalid("atr sütunu yok".into()))?,
            genome: genome.clone(),
            src,
            trades_today: (u64::MAX, 0),
        })
    }

    /// Backtest: önceden hesaplanmış paylaşılan sütunlarla.
    pub fn with_columns(
        genome: &Genome,
        inst: InstrumentId,
        tick_value: Money,
        shared: &SharedColumns,
    ) -> Result<Self, GenomeError> {
        let (specs, map) = Self::columns_of(genome)?;
        let cols = specs
            .iter()
            .map(|s| {
                shared
                    .cols
                    .get(s)
                    .cloned()
                    .ok_or_else(|| GenomeError::Invalid(format!("{s} sütunu hesaplanmamış")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::build(
            genome,
            inst,
            tick_value,
            &map,
            Source::Shared {
                times: shared.available_at.clone(),
                cols,
                cursor: 0,
            },
        )
    }

    /// Canlı/gölge: barlar geldikçe feature'ları kendisi hesaplar.
    pub fn live(
        genome: &Genome,
        inst: InstrumentId,
        tick_value: Money,
        session: SessionSpec,
    ) -> Result<Self, GenomeError> {
        let (specs, map) = Self::columns_of(genome)?;
        let frame = FeatureFrame::new(&specs, session);
        let order = specs
            .iter()
            .map(|s| frame.index_of(s).unwrap_or(0))
            .collect();
        Self::build(
            genome,
            inst,
            tick_value,
            &map,
            Source::Live {
                frame: Box::new(frame),
                order,
            },
        )
    }

    pub fn genome(&self) -> &Genome {
        &self.genome
    }

    /// Bu barın sütun indeksi (paylaşılan kaynakta kapanış zamanıyla eşleşir; canlıda eklenir).
    fn index_for(&mut self, close_ts: UnixNanos, bar: &atlas_core::Bar) -> Option<usize> {
        match &mut self.src {
            Source::Shared { times, cursor, .. } => {
                while *cursor < times.len() && times[*cursor] < close_ts {
                    *cursor += 1;
                }
                (*cursor < times.len() && times[*cursor] == close_ts).then_some(*cursor)
            }
            Source::Live { frame, .. } => {
                frame.push(bar);
                frame.len().checked_sub(1)
            }
        }
    }

    /// (atr, yön) — her barda yığın ayırması olmadan, somut okuyucuyla.
    fn decide(&self, i: usize) -> Option<(f64, Side)> {
        fn go<V: Values>(s: &GenomeStrategy, v: &V, i: usize) -> Option<(f64, Side)> {
            let atr = v.get(s.atr_col, i)?;
            if s.genome.filters.min_atr_ticks.is_some_and(|m| atr < m) {
                return None;
            }
            if s.long.as_ref().is_some_and(|c| eval_cond(c, v, i)) {
                Some((atr, Side::Buy))
            } else if s.short.as_ref().is_some_and(|c| eval_cond(c, v, i)) {
                Some((atr, Side::Sell))
            } else {
                None
            }
        }
        match &self.src {
            Source::Shared { cols, .. } => go(self, &SharedView { cols }, i),
            Source::Live { frame, order } => go(self, &LiveView { frame, order }, i),
        }
    }
}

impl Strategy for GenomeStrategy {
    fn id(&self) -> &StrategyId {
        &self.id
    }

    fn on_event(&mut self, ctx: &Ctx<'_>, ev: &MarketEvent) -> Vec<Intent> {
        if ev.instrument != self.inst {
            return Vec::new();
        }
        let MarketPayload::Bar(bar) = &ev.payload else {
            return Vec::new();
        };
        let Some(i) = self.index_for(bar.close_ts, bar) else {
            return Vec::new();
        };
        // Zaman noktalı güvence: bu barın değeri şu an kullanılabilir olmalı.
        if bar.close_ts > ctx.now() || !ctx.is_flat(self.inst) || ctx.feed_down(self.inst) {
            return Vec::new();
        }
        let f = &self.genome.filters;
        let now = ctx.now();
        if let Some((a, b)) = f.hours_utc {
            let h = ((now.0 % NANOS_PER_DAY) / NANOS_PER_HOUR) as u32;
            let inside = if a <= b {
                h >= a && h < b
            } else {
                h >= a || h < b
            };
            if !inside {
                return Vec::new();
            }
        }
        if let Some(m) = f.no_entry_before_week_close_min
            && ctx
                .time_to_week_close(self.inst)
                .is_some_and(|t| t < u64::from(m) * NANOS_PER_MIN)
        {
            return Vec::new();
        }
        let day = now.0 / NANOS_PER_DAY;
        if self.trades_today.0 != day {
            self.trades_today = (day, 0);
        }
        if let Some(mx) = f.max_trades_per_day
            && self.trades_today.1 >= mx
        {
            return Vec::new();
        }
        let Some((atr, side)) = self.decide(i) else {
            return Vec::new();
        };
        let ex = &self.genome.exit;
        let stop_ticks = ((ex.stop_atr * atr).round() as i64).max(1);
        let risk_micros =
            i64::from(self.genome.sizing.risk_usd) * atlas_core::units::MICROS_PER_USD;
        let per_contract = stop_ticks.saturating_mul(self.tick_value.0).max(1);
        let qty = risk_micros / per_contract;
        if qty < 1 {
            return Vec::new();
        }
        let qty = i32::try_from(qty).unwrap_or(i32::MAX);
        let sg = i64::from(side.sign());
        let close = bar.close;
        let bar_ns = bar.close_ts.since(bar.open_ts).max(1);
        self.trades_today.1 += 1;
        vec![Intent {
            strategy: self.id.clone(),
            instrument: self.inst,
            ts: now,
            action: Action::Open {
                side,
                qty,
                entry: EntryType::Market,
                stop_loss: Price(close.0 - sg * stop_ticks),
                take_profit: ex
                    .target_r
                    .map(|r| Price(close.0 + sg * ((r * stop_ticks as f64).round() as i64).max(1))),
                time_exit: ex.time_bars.map(|n| now.plus(u64::from(n) * bar_ns)),
                entry_expiry: None,
            },
        }]
    }
}

/// Referanslardan yalnız `FeatureRef` listesini döndürür (rapor/izleme).
pub fn refs(genome: &Genome) -> Result<Vec<FeatureRef>, GenomeError> {
    genome.feature_refs()
}
