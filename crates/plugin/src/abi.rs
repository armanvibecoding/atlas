//! Host ↔ plugin veri sözleşmesi (ABI v1): girdi görünümü ve çıktı şeması.
//!
//! Tüm fiyatlar enstrümanın tick'i cinsinden tam sayıdır (kural 1); zamanlar UTC nanosaniyedir.

use crate::PluginLimits;
use atlas_core::{
    Action, Bar, Ctx, EntryType, InstrumentId, Intent, Price, Side, StrategyId, UnixNanos,
};
use serde::{Deserialize, Serialize};

/// Girdi şemasının sürümü; plugin farklı sürüm görürse intent döndürmemelidir.
pub const ABI_VERSION: u32 = 1;

/// Tek bar (OHLCV, tick cinsinden).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BarInput {
    pub open_ts: u64,
    pub close_ts: u64,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub volume: u64,
}

impl From<&Bar> for BarInput {
    fn from(b: &Bar) -> Self {
        BarInput {
            open_ts: b.open_ts.0,
            close_ts: b.close_ts.0,
            open: b.open.0,
            high: b.high.0,
            low: b.low.0,
            close: b.close.0,
            volume: b.volume,
        }
    }
}

/// Stratejinin bu enstrümandaki açık pozisyonu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionInput {
    pub side: Side,
    pub qty: i32,
    pub entry: i64,
    pub stop: i64,
}

/// Plugin'e verilen görünüm. `ctx`'ten kurulur; olay anına kadarki veriyi içerir (kural 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInput {
    pub abi: u32,
    pub instrument: u32,
    /// Şimdi (= olay anı, UTC ns).
    pub now: u64,
    /// Son N bar, eskiden yeniye; sonuncusu bu olayın barıdır.
    pub bars: Vec<BarInput>,
    pub position: Option<PositionInput>,
    /// Bekleyen (dolmamış) giriş emri var mı?
    pub pending_entry: bool,
    /// Hafta kapanışına kalan ns (ders §9.6); bilinmiyorsa `null`.
    pub week_close_ns: Option<u64>,
}

impl PluginInput {
    /// `ctx`'ten görünüm kurar.
    pub fn from_ctx(ctx: &Ctx<'_>, inst: InstrumentId, window_bars: usize) -> Self {
        let bars = ctx.bars(inst);
        let from = bars.len().saturating_sub(window_bars);
        PluginInput {
            abi: ABI_VERSION,
            instrument: inst.0,
            now: ctx.now().0,
            bars: bars
                .get(from..)
                .unwrap_or(&[])
                .iter()
                .map(BarInput::from)
                .collect(),
            position: ctx.position(inst).map(|p| PositionInput {
                side: p.side,
                qty: p.qty,
                entry: p.entry.0,
                stop: p.stop.0,
            }),
            pending_entry: ctx.has_pending_entry(inst),
            week_close_ns: ctx.time_to_week_close(inst),
        }
    }
}

/// Plugin çıktısındaki tek intent. Şema katıdır: bilinmeyen alan veya eylem = geçersiz çıktı.
///
/// ```json
/// [{"action":"open","side":"buy","qty":1,"stop_loss":19950,"take_profit":20100,"time_exit_bars":30}]
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginIntent {
    /// Piyasa emriyle pozisyon aç. Stop zorunlu (mutlak fiyat, tick); hedef ve zaman çıkışı opsiyonel.
    Open {
        side: Side,
        qty: i32,
        stop_loss: i64,
        #[serde(default)]
        take_profit: Option<i64>,
        #[serde(default)]
        time_exit_bars: Option<u32>,
    },
    /// Pozisyonu kapat (gerekçe metnini host yazar; plugin metni journal'a girmez).
    Close,
    /// Bekleyen giriş emrini iptal et.
    CancelEntry,
}

/// Geçersiz çıktı nedeni.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutputError {
    #[error("çıktı şemaya uymuyor: {0}")]
    Schema(String),
    #[error("çok fazla intent: {n} > {max}")]
    TooMany { n: usize, max: usize },
    #[error("bir olayda birden fazla open")]
    MultipleOpen,
    #[error("open ama strateji düz değil (pozisyon ya da bekleyen giriş var)")]
    NotFlat,
    #[error("geçersiz adet: {0}")]
    Qty(i32),
    #[error("stop yanlış tarafta: stop {stop}, referans {reference}")]
    StopSide { stop: i64, reference: i64 },
    #[error("hedef yanlış tarafta: hedef {target}, referans {reference}")]
    TargetSide { target: i64, reference: i64 },
    #[error("geçersiz time_exit_bars: {0}")]
    TimeExit(u32),
    #[error("referans bar yok")]
    NoReference,
}

/// Çıktıyı parse eder ve doğrular. Bir öğe bile geçersizse tamamı reddedilir.
///
/// `input` bu çağrıya verilen görünümdür: referans fiyat son barın kapanışıdır, düzlük ondan okunur.
pub fn parse_output(
    bytes: &[u8],
    input: &PluginInput,
    limits: &PluginLimits,
) -> Result<Vec<PluginIntent>, OutputError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let list: Vec<PluginIntent> =
        serde_json::from_slice(bytes).map_err(|e| OutputError::Schema(e.to_string()))?;
    if list.len() > limits.max_intents {
        return Err(OutputError::TooMany {
            n: list.len(),
            max: limits.max_intents,
        });
    }
    let opens = list
        .iter()
        .filter(|i| matches!(i, PluginIntent::Open { .. }))
        .count();
    if opens > 1 {
        return Err(OutputError::MultipleOpen);
    }
    for it in &list {
        let PluginIntent::Open {
            side,
            qty,
            stop_loss,
            take_profit,
            time_exit_bars,
        } = *it
        else {
            continue;
        };
        if input.position.is_some() || input.pending_entry {
            return Err(OutputError::NotFlat);
        }
        if qty < 1 || qty > limits.max_qty {
            return Err(OutputError::Qty(qty));
        }
        let reference = input.bars.last().ok_or(OutputError::NoReference)?.close;
        let sg = i64::from(side.sign());
        // Alışta stop referansın altında, hedef üstünde; satışta tersi.
        if (reference - stop_loss).saturating_mul(sg) <= 0 {
            return Err(OutputError::StopSide {
                stop: stop_loss,
                reference,
            });
        }
        if let Some(t) = take_profit
            && (t - reference).saturating_mul(sg) <= 0
        {
            return Err(OutputError::TargetSide {
                target: t,
                reference,
            });
        }
        if let Some(n) = time_exit_bars
            && (n == 0 || n > limits.max_time_exit_bars)
        {
            return Err(OutputError::TimeExit(n));
        }
    }
    Ok(list)
}

/// Doğrulanmış plugin intent'lerinden `Intent` kurar. Kimlik, enstrüman ve zaman host'tandır.
pub(crate) fn to_intents(
    list: Vec<PluginIntent>,
    strategy: &StrategyId,
    inst: InstrumentId,
    now: UnixNanos,
    bar_ns: u64,
) -> Vec<Intent> {
    list.into_iter()
        .map(|it| {
            let action = match it {
                PluginIntent::Open {
                    side,
                    qty,
                    stop_loss,
                    take_profit,
                    time_exit_bars,
                } => Action::Open {
                    side,
                    qty,
                    entry: EntryType::Market,
                    stop_loss: Price(stop_loss),
                    take_profit: take_profit.map(Price),
                    time_exit: time_exit_bars
                        .map(|n| now.plus(u64::from(n).saturating_mul(bar_ns.max(1)))),
                    entry_expiry: None,
                },
                PluginIntent::Close => Action::Close {
                    reason: "plugin".into(),
                },
                PluginIntent::CancelEntry => Action::CancelEntry,
            };
            Intent {
                strategy: strategy.clone(),
                instrument: inst,
                ts: now,
                action,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(close: i64) -> PluginInput {
        PluginInput {
            abi: ABI_VERSION,
            instrument: 0,
            now: 120,
            bars: vec![BarInput {
                open_ts: 60,
                close_ts: 120,
                open: close,
                high: close + 1,
                low: close - 1,
                close,
                volume: 10,
            }],
            position: None,
            pending_entry: false,
            week_close_ns: None,
        }
    }

    fn parse(s: &str) -> Result<Vec<PluginIntent>, OutputError> {
        parse_output(s.as_bytes(), &input(100), &PluginLimits::default())
    }

    #[test]
    fn valid_open_parses() {
        let v = parse(
            r#"[{"action":"open","side":"buy","qty":2,"stop_loss":90,"take_profit":120,"time_exit_bars":5}]"#,
        )
        .unwrap();
        assert_eq!(
            v,
            vec![PluginIntent::Open {
                side: Side::Buy,
                qty: 2,
                stop_loss: 90,
                take_profit: Some(120),
                time_exit_bars: Some(5)
            }]
        );
        let v = parse(r#"[{"action":"open","side":"sell","qty":1,"stop_loss":101}]"#).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(parse("[]").unwrap(), vec![]);
        assert_eq!(parse("").unwrap(), vec![]);
    }

    #[test]
    fn schema_violations_rejected() {
        for bad in [
            "{",
            "null",
            r#"{"action":"open"}"#,
            r#"[{"action":"buy_everything"}]"#,
            r#"[{"action":"open","side":"buy","qty":1}]"#,
            r#"[{"action":"open","side":"long","qty":1,"stop_loss":90}]"#,
            r#"[{"action":"open","side":"buy","qty":1,"stop_loss":90,"extra":1}]"#,
            r#"[{"action":"open","side":"buy","qty":1.5,"stop_loss":90}]"#,
            r#"[{"action":"open","side":"buy","qty":99999999999,"stop_loss":90}]"#,
            r#"[{"action":"open","side":"buy","qty":1,"stop_loss":"90"}]"#,
        ] {
            assert!(
                matches!(parse(bad), Err(OutputError::Schema(_))),
                "kabul edilmemeli: {bad}"
            );
        }
    }

    #[test]
    fn semantic_violations_rejected() {
        let e = |s: &str| parse(s).unwrap_err();
        assert_eq!(
            e(r#"[{"action":"open","side":"buy","qty":0,"stop_loss":90}]"#),
            OutputError::Qty(0)
        );
        assert_eq!(
            e(r#"[{"action":"open","side":"buy","qty":1001,"stop_loss":90}]"#),
            OutputError::Qty(1001)
        );
        assert!(matches!(
            e(r#"[{"action":"open","side":"buy","qty":1,"stop_loss":100}]"#),
            OutputError::StopSide { .. }
        ));
        assert!(matches!(
            e(r#"[{"action":"open","side":"sell","qty":1,"stop_loss":95}]"#),
            OutputError::StopSide { .. }
        ));
        assert!(matches!(
            e(r#"[{"action":"open","side":"buy","qty":1,"stop_loss":90,"take_profit":99}]"#),
            OutputError::TargetSide { .. }
        ));
        assert_eq!(
            e(r#"[{"action":"open","side":"buy","qty":1,"stop_loss":90,"time_exit_bars":0}]"#),
            OutputError::TimeExit(0)
        );
        assert_eq!(
            e(
                r#"[{"action":"open","side":"buy","qty":1,"stop_loss":90},{"action":"open","side":"buy","qty":1,"stop_loss":90}]"#
            ),
            OutputError::MultipleOpen
        );
        let many = format!("[{}]", [r#"{"action":"cancel_entry"}"#; 9].join(","));
        assert_eq!(e(&many), OutputError::TooMany { n: 9, max: 8 });

        let mut inp = input(100);
        inp.pending_entry = true;
        assert_eq!(
            parse_output(
                br#"[{"action":"open","side":"buy","qty":1,"stop_loss":90}]"#,
                &inp,
                &PluginLimits::default()
            ),
            Err(OutputError::NotFlat)
        );
    }

    #[test]
    fn host_builds_intent_identity_and_time() {
        let id = StrategyId::new("plugin:x#abc");
        let v = to_intents(
            vec![
                PluginIntent::Open {
                    side: Side::Sell,
                    qty: 3,
                    stop_loss: 110,
                    take_profit: None,
                    time_exit_bars: Some(4),
                },
                PluginIntent::Close,
            ],
            &id,
            InstrumentId(7),
            UnixNanos(1_000),
            60,
        );
        assert_eq!(v.len(), 2);
        assert!(v.iter().all(|i| i.strategy == id
            && i.instrument == InstrumentId(7)
            && i.ts == UnixNanos(1_000)));
        assert_eq!(
            v[0].action,
            Action::Open {
                side: Side::Sell,
                qty: 3,
                entry: EntryType::Market,
                stop_loss: Price(110),
                take_profit: None,
                time_exit: Some(UnixNanos(1_240)),
                entry_expiry: None,
            }
        );
        assert_eq!(
            v[1].action,
            Action::Close {
                reason: "plugin".into()
            }
        );
    }
}
