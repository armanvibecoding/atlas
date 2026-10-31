//! Dolum modelleri (`FillModel`): `BarFill` (hızlı tarama) ve `TradeFill`.
//!
//! Kurallar (M1 kabul, §4.4):
//! - Piyasa emri bir sonraki olayın fiyatından (bar: açılış) + kayma.
//! - Açılış stop'un ötesindeyse (gap) stop **açılış fiyatından** dolar.
//! - Aynı barda hem stop hem hedef değdiyse **stop** dolar (karamsar).
//! - Limit emir varsayılan olarak fiyat **içinden geçince** dolar (dokunma yetmez).

use atlas_core::{
    Bar, FillReason, InstrumentId, MarketEvent, MarketPayload, Price, Side, StrategyId, Trade,
    UnixNanos,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderKind {
    Market,
    Limit(Price),
    Stop(Price),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Entry,
    StopLoss,
    TakeProfit,
    /// Motorun ürettiği piyasa çıkışı (zaman, kill, seans sonu, strateji kapatması).
    Exit(FillReason),
}

impl Role {
    pub fn fill_reason(self) -> FillReason {
        match self {
            Role::Entry => FillReason::Entry,
            Role::StopLoss => FillReason::StopLoss,
            Role::TakeProfit => FillReason::TakeProfit,
            Role::Exit(r) => r,
        }
    }
    pub fn is_exit(self) -> bool {
        !matches!(self, Role::Entry)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkingOrder {
    pub id: u64,
    pub strategy: StrategyId,
    pub instrument: InstrumentId,
    pub side: Side,
    pub qty: i32,
    pub kind: OrderKind,
    pub role: Role,
    /// Emir bu andan önceki olaylarla eşleşmez (gecikme modeli).
    pub active_from: UnixNanos,
    /// Emrin oluşturulduğu olay sırası; aynı olayla eşleşmez.
    pub created_seq: u64,
    pub expiry: Option<UnixNanos>,
}

/// Bir emrin bu olayda tetiklenmesi.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trigger {
    pub order_id: u64,
    /// Kayma dahil dolum fiyatı.
    pub price: Price,
    /// Kaymasız referans fiyat.
    pub reference: Price,
    /// Olayın ilk fiyatında (bar açılışı) mı tetiklendi?
    pub at_open: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LimitRule {
    /// Fiyat limitin içinden geçmeli (karamsar, varsayılan).
    TradeThrough,
    /// Dokunma yeter (yalnız doğrulama testlerinde, sapmasız referans için).
    Touch,
}

/// Bar içi eşleştirme kapsamı.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarScope {
    /// Emir bar açılmadan önce aktifti: tüm bar (açılış dahil).
    Full,
    /// Giriş bar içinde (açılış sonrası bilinmeyen anda) doldu: yalnız stop, açılış hariç.
    AfterIntrabarEntry,
}

pub trait FillModel {
    fn name(&self) -> &'static str;
    /// Kalibrasyon sürümü; deney kaydı bunu taşır (§4.4).
    fn version(&self) -> u32;
    fn handles(&self, payload: &MarketPayload) -> bool;
    /// Tek emir bu olayla tetikleniyor mu? (Sıcak yol: bellek ayırmaz.)
    fn match_one(
        &self,
        ev: &MarketEvent,
        order: &WorkingOrder,
        slippage_ticks: i64,
        scope: BarScope,
    ) -> Option<Trigger>;

    /// `orders` bu olayın enstrümanına ait, aktif emirlerdir. Tetiklenenleri döner.
    fn match_orders(
        &self,
        ev: &MarketEvent,
        orders: &[&WorkingOrder],
        slippage_ticks: i64,
        scope: BarScope,
    ) -> Vec<Trigger> {
        orders
            .iter()
            .filter_map(|o| self.match_one(ev, o, slippage_ticks, scope))
            .collect()
    }
}

/// Bir yön için kaymayı uygula (her zaman aleyhte).
fn slip(side: Side, p: Price, ticks: i64) -> Price {
    match side {
        Side::Buy => p + ticks,
        Side::Sell => p - ticks,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BarFill {
    pub limit_rule: LimitRule,
}

impl Default for BarFill {
    fn default() -> Self {
        BarFill {
            limit_rule: LimitRule::TradeThrough,
        }
    }
}

impl BarFill {
    fn one(&self, b: &Bar, o: &WorkingOrder, s: i64, scope: BarScope) -> Option<Trigger> {
        let t = |price: Price, reference: Price, at_open: bool| Trigger {
            order_id: o.id,
            price,
            reference,
            at_open,
        };
        if scope == BarScope::AfterIntrabarEntry {
            // Yalnız stop; girişten sonraki yol bilinmediği için hedef sayılmaz (karamsar).
            return match o.kind {
                OrderKind::Stop(p) => {
                    let hit = match o.side {
                        Side::Buy => b.high >= p,
                        Side::Sell => b.low <= p,
                    };
                    hit.then(|| t(slip(o.side, p, s), p, false))
                }
                _ => None,
            };
        }
        match o.kind {
            OrderKind::Market => Some(t(slip(o.side, b.open, s), b.open, true)),
            OrderKind::Stop(p) => match o.side {
                Side::Buy if b.open >= p => Some(t(slip(o.side, b.open, s), b.open, true)),
                Side::Buy if b.high >= p => Some(t(slip(o.side, p, s), p, false)),
                Side::Sell if b.open <= p => Some(t(slip(o.side, b.open, s), b.open, true)),
                Side::Sell if b.low <= p => Some(t(slip(o.side, p, s), p, false)),
                _ => None,
            },
            OrderKind::Limit(p) => {
                let touch = self.limit_rule == LimitRule::Touch;
                match o.side {
                    Side::Buy if b.open <= p => Some(t(b.open, b.open, true)),
                    Side::Buy if b.low < p || (touch && b.low <= p) => Some(t(p, p, false)),
                    Side::Sell if b.open >= p => Some(t(b.open, b.open, true)),
                    Side::Sell if b.high > p || (touch && b.high >= p) => Some(t(p, p, false)),
                    _ => None,
                }
            }
        }
    }
}

impl FillModel for BarFill {
    fn name(&self) -> &'static str {
        "bar_fill"
    }
    fn version(&self) -> u32 {
        1
    }
    fn handles(&self, payload: &MarketPayload) -> bool {
        matches!(payload, MarketPayload::Bar(_))
    }
    fn match_one(
        &self,
        ev: &MarketEvent,
        o: &WorkingOrder,
        s: i64,
        scope: BarScope,
    ) -> Option<Trigger> {
        let MarketPayload::Bar(b) = &ev.payload else {
            return None;
        };
        self.one(b, o, s, scope)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TradeFill {
    pub limit_rule: LimitRule,
}

impl Default for TradeFill {
    fn default() -> Self {
        TradeFill {
            limit_rule: LimitRule::TradeThrough,
        }
    }
}

impl TradeFill {
    fn one(&self, tr: &Trade, o: &WorkingOrder, s: i64) -> Option<Trigger> {
        let x = tr.price;
        let t = |price: Price, reference: Price| Trigger {
            order_id: o.id,
            price,
            reference,
            at_open: false,
        };
        let touch = self.limit_rule == LimitRule::Touch;
        match (o.kind, o.side) {
            (OrderKind::Market, side) => Some(t(slip(side, x, s), x)),
            (OrderKind::Stop(p), Side::Buy) if x >= p => Some(t(slip(Side::Buy, x, s), x)),
            (OrderKind::Stop(p), Side::Sell) if x <= p => Some(t(slip(Side::Sell, x, s), x)),
            (OrderKind::Limit(p), Side::Buy) if x < p || (touch && x <= p) => Some(t(p, p)),
            (OrderKind::Limit(p), Side::Sell) if x > p || (touch && x >= p) => Some(t(p, p)),
            _ => None,
        }
    }
}

impl FillModel for TradeFill {
    fn name(&self) -> &'static str {
        "trade_fill"
    }
    fn version(&self) -> u32 {
        1
    }
    fn handles(&self, payload: &MarketPayload) -> bool {
        matches!(payload, MarketPayload::Trade(_))
    }
    fn match_one(
        &self,
        ev: &MarketEvent,
        o: &WorkingOrder,
        s: i64,
        _scope: BarScope,
    ) -> Option<Trigger> {
        let MarketPayload::Trade(tr) = &ev.payload else {
            return None;
        };
        if o.active_from > ev.ts {
            return None;
        }
        self.one(tr, o, s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(o: i64, h: i64, l: i64, c: i64) -> MarketEvent {
        MarketEvent::bar(
            InstrumentId(0),
            Bar {
                open_ts: UnixNanos(0),
                close_ts: UnixNanos(60),
                open: Price(o),
                high: Price(h),
                low: Price(l),
                close: Price(c),
                volume: 1,
                buy_volume: 0,
                trades: 1,
            },
        )
    }

    fn order(id: u64, side: Side, kind: OrderKind, role: Role) -> WorkingOrder {
        WorkingOrder {
            id,
            strategy: StrategyId::new("s"),
            instrument: InstrumentId(0),
            side,
            qty: 1,
            kind,
            role,
            active_from: UnixNanos(0),
            created_seq: 0,
            expiry: None,
        }
    }

    #[test]
    fn market_fills_at_open_plus_slippage() {
        let m = BarFill::default();
        let o = order(1, Side::Buy, OrderKind::Market, Role::Entry);
        let t = m.match_orders(&bar(100, 105, 95, 101), &[&o], 2, BarScope::Full);
        assert_eq!(t[0].price, Price(102));
        assert_eq!(t[0].reference, Price(100));
        assert!(t[0].at_open);
    }

    #[test]
    fn gap_stop_fills_at_open() {
        let m = BarFill::default();
        // long'un stopu 95; bar 90'dan açılıyor
        let o = order(1, Side::Sell, OrderKind::Stop(Price(95)), Role::StopLoss);
        let t = m.match_orders(&bar(90, 92, 85, 91), &[&o], 0, BarScope::Full);
        assert_eq!(t[0].price, Price(90));
        assert!(t[0].at_open);
        // gap yoksa stop fiyatından
        let t = m.match_orders(&bar(100, 101, 94, 96), &[&o], 1, BarScope::Full);
        assert_eq!(t[0].price, Price(94));
        assert_eq!(t[0].reference, Price(95));
    }

    #[test]
    fn limit_requires_trade_through_by_default() {
        let m = BarFill::default();
        let o = order(
            1,
            Side::Sell,
            OrderKind::Limit(Price(105)),
            Role::TakeProfit,
        );
        assert!(
            m.match_orders(&bar(100, 105, 99, 104), &[&o], 0, BarScope::Full)
                .is_empty()
        );
        let t = m.match_orders(&bar(100, 106, 99, 104), &[&o], 0, BarScope::Full);
        assert_eq!(t[0].price, Price(105));
        let touch = BarFill {
            limit_rule: LimitRule::Touch,
        };
        assert_eq!(
            touch
                .match_orders(&bar(100, 105, 99, 104), &[&o], 0, BarScope::Full)
                .len(),
            1
        );
        // olumlu gap: açılış limitin ötesinde → açılıştan
        let t = m.match_orders(&bar(108, 110, 107, 109), &[&o], 0, BarScope::Full);
        assert_eq!(t[0].price, Price(108));
    }

    #[test]
    fn after_intrabar_entry_only_stop_counts() {
        let m = BarFill::default();
        let sl = order(1, Side::Sell, OrderKind::Stop(Price(95)), Role::StopLoss);
        let tp = order(
            2,
            Side::Sell,
            OrderKind::Limit(Price(105)),
            Role::TakeProfit,
        );
        let t = m.match_orders(
            &bar(100, 110, 99, 104),
            &[&sl, &tp],
            0,
            BarScope::AfterIntrabarEntry,
        );
        assert!(t.is_empty());
        let t = m.match_orders(
            &bar(100, 110, 90, 104),
            &[&sl, &tp],
            0,
            BarScope::AfterIntrabarEntry,
        );
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].order_id, 1);
    }

    #[test]
    fn trade_fill_rules() {
        let m = TradeFill::default();
        let ev = MarketEvent::trade(
            InstrumentId(0),
            UnixNanos(10),
            Trade {
                price: Price(100),
                size: 1,
                aggressor: None,
            },
        );
        let mk = order(1, Side::Sell, OrderKind::Market, Role::Entry);
        let st = order(2, Side::Buy, OrderKind::Stop(Price(99)), Role::StopLoss);
        let lm = order(3, Side::Buy, OrderKind::Limit(Price(100)), Role::Entry);
        let mut late = order(4, Side::Sell, OrderKind::Market, Role::Entry);
        late.active_from = UnixNanos(11);
        let t = m.match_orders(&ev, &[&mk, &st, &lm, &late], 1, BarScope::Full);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].price, Price(99)); // satış piyasa − kayma
        assert_eq!(t[1].price, Price(101)); // stop tetik: işlem fiyatı + kayma
    }
}
