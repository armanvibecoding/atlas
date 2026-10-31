//! `MboQueueFill`: limit emirde kuyruk pozisyonu, piyasa/stop emirde kayma + basit piyasa etkisi (§4.4).
//!
//! ## Veri durumu: L1 tahmini, MBO gelince kesinleşir
//! Gerçek MBO (emir bazlı kitap) verisi henüz yok (Databento hesabı, M0). Model bu yüzden yalnız
//! L1 kotasyon (`Quote`: en iyi alış/satış + görünür boyut) ve işlem (`Trade`) akışıyla **önündeki
//! kuyruğu tahmin eder**. Tahmin her adımda bir *üst sınırdır* (karamsar): gerçek kuyruk ancak daha
//! kısa olabilir. MBO geldiğinde aynı arayüz (`FillModel`) korunur; değişen yalnız kuyruk durumunun
//! kaynağıdır: önündeki hacim, emir kimlikleriyle birebir sayılır (iptaller önde mi arkada mı
//! kesin bilinir), görünür-boyut üst sınırı ve "bilinmeyen kuyruk" hâli ortadan kalkar.
//!
//! ## Limit emir kuralları
//! Emir ilk kez görüldüğünde (aktif olduktan sonraki ilk olay) son kotasyona göre:
//! - Karşı tarafın en iyi fiyatı limite eşit ya da daha iyiyse emir **pazarlanabilir**: karşı en iyi
//!   fiyattan (kaymasız) hemen dolar.
//! - Fiyat kendi tarafın en iyisinden **daha iyiyse** önündeki kuyruk 0.
//! - Fiyat kendi tarafın en iyisine **eşitse** önündeki kuyruk = o taraftaki görünür boyut.
//! - Fiyat kitabın **içindeyse** (daha kötü) görünür boyut bilinmez: kuyruk *bilinmiyor* kabul
//!   edilir ve yalnız fiyat limitin içinden geçince dolar. Seviye sonradan en iyi fiyat olursa kuyruk
//!   o andaki görünür boyutla (üst sınır) bilinir hâle gelir. Hiç kotasyon görülmemişse de aynı.
//!
//! Sonraki olaylarda:
//! - Limit fiyatında gerçekleşen, bizim tarafımıza vuran işlem (agresör karşı taraf ya da bilinmiyor)
//!   önündeki kuyruğu işlem boyutu kadar azaltır; kuyruk tükenince emir limit fiyatından dolar.
//! - İşlem fiyatı limitin içinden geçerse (alışta limitin altı) kuyruk ne olursa olsun dolum.
//! - Karşı tarafın en iyi fiyatı limitin içine girerse (alışta ask < limit) dolum.
//! - Seviye en iyi fiyatken görünür boyut tahminden küçükse tahmin o boyuta indirilir (iptaller).
//! - Seviye tamamen kaybolursa (alışta bid < limit < ask) önümüzde kimse kalmamıştır: kuyruk 0.
//!
//! Kalibre edilen limit dolum olasılığı `p < 1` ise görünür kuyruk `1/p` ile şişirilir
//! (deterministik; canlı, simülasyonun "doldu" dediği limitlerin yalnız `p` kadarını dolduruyorsa
//! simülasyon o kadar geç dolmalı).
//!
//! ## Piyasa ve stop emirleri
//! İşlem fiyatı (piyasa emri kotasyonda gelirse karşı en iyi fiyat) + kayma + piyasa etkisi.
//! Etki: `ceil(impact_milli_ticks_per_lot × (adet − 1) / 1000)` tick; 1 lot etkisizdir (temel kayma
//! onu zaten kapsar). Stop yalnız işlemle tetiklenir (CME stop'u işlem fiyatıyla tetikler).
//!
//! ## Gecikme
//! `WorkingOrder::active_from` öncesindeki olaylar emirle eşleşmez (motorun gecikme modeli).
//!
//! ## Sınırlamalar
//! - Kısmi dolum yok: kuyruk tükendiğinde emrin tamamı dolar (adet > 1 için iyimser).
//! - Model **koşu başına durumludur** (emir kimliği → kuyruk). Her simülasyon koşusu yeni örnek
//!   (`MboQueueFill::new` / `reset`) kullanmalıdır; emir kimlikleri koşular arasında tekrarlanır.
//! - `match_one` her olay için her emirde en fazla bir kez çağrılmalıdır (aynı işlem iki kez
//!   kuyruktan düşülmesin). Motor bu sözleşmeyi işlem/kotasyon olaylarında sağlar.
//! - Motor `observe` çağırmadıkça son kotasyon yalnız o enstrümanda aktif emir varken öğrenilir;
//!   eksik bilgi karamsar yöne düşer (bilinmeyen kuyruk → yalnız içinden geçiş).

use crate::calibration::FillParams;
use crate::fill::{BarScope, FillModel, OrderKind, Trigger, WorkingOrder};
use atlas_core::{InstrumentId, MarketEvent, MarketPayload, Price, Quote, Side};
use std::cell::RefCell;
use std::collections::BTreeMap;

/// Modelin adı; deney kaydında `queue@v<sürüm>` olarak görünür.
pub const QUEUE_MODEL_NAME: &str = "queue";

/// Kuyruk modelinin ayarları (kalibre parametrelerden türetilir).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueConfig {
    /// Kalibrasyon sürümü (`FillParams::version`).
    pub version: u32,
    /// Lot başına ek kayma, binde tick (500 = lot başına yarım tick).
    pub impact_milli_ticks_per_lot: i64,
    /// Limit dolum olasılığı, binde (1..=1000). 1000 = kuyruk şişirilmez.
    pub limit_fill_prob_milli: i64,
}

impl Default for QueueConfig {
    fn default() -> Self {
        QueueConfig {
            version: 1,
            impact_milli_ticks_per_lot: 0,
            limit_fill_prob_milli: 1000,
        }
    }
}

/// Bir limit emrin kuyruk durumu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QueueState {
    side: Side,
    price: Price,
    /// Önündeki tahmini hacim (lot). `None` = bilinmiyor (yalnız içinden geçişle dolum).
    ahead: Option<u64>,
}

#[derive(Debug, Default)]
struct BookState {
    quotes: BTreeMap<InstrumentId, Quote>,
    queues: BTreeMap<u64, QueueState>,
}

/// İlk görüşte limit emrin durumu.
enum Init {
    /// Karşı en iyi fiyat limite eşit/daha iyi: bu fiyattan hemen dolar.
    Marketable(Price),
    Resting(Option<u64>),
}

#[derive(Debug, Default)]
pub struct MboQueueFill {
    cfg: QueueConfig,
    state: RefCell<BookState>,
}

impl MboQueueFill {
    pub fn new(cfg: QueueConfig) -> Self {
        MboQueueFill {
            cfg,
            state: RefCell::new(BookState::default()),
        }
    }

    /// Kalibre parametrelerden model. Kayma tick'leri motorun enstrüman kaymasıdır (bkz. ADR 0006);
    /// buradan sürüm, piyasa etkisi ve limit dolum olasılığı alınır.
    pub fn from_params(p: &FillParams) -> Self {
        Self::new(QueueConfig {
            version: p.version,
            impact_milli_ticks_per_lot: p.impact_milli_ticks_per_lot,
            limit_fill_prob_milli: p.limit_fill_prob_milli(),
        })
    }

    pub fn config(&self) -> QueueConfig {
        self.cfg
    }

    /// Yeni koşu için tüm durumu temizler.
    pub fn reset(&self) {
        if let Ok(mut s) = self.state.try_borrow_mut() {
            *s = BookState::default();
        }
    }

    /// Teşhis/test: emrin önündeki tahmini kuyruk. Dış `None` = emir henüz görülmedi;
    /// iç `None` = kuyruk bilinmiyor.
    pub fn queue_ahead(&self, order_id: u64) -> Option<Option<u64>> {
        let s = self.state.try_borrow().ok()?;
        s.queues.get(&order_id).map(|q| q.ahead)
    }

    /// Adede bağlı piyasa etkisi (tick, ≥ 0). 1 lot → 0; yukarı yuvarlanır (karamsar).
    pub fn impact_ticks(&self, qty: i32) -> i64 {
        impact_ticks(self.cfg.impact_milli_ticks_per_lot, qty)
    }

    /// Görünür kuyruğu dolum olasılığına göre şişirir: `ceil(boyut × 1000 / p_milli)`.
    fn inflate(&self, size: u64) -> u64 {
        let p = u64::try_from(self.cfg.limit_fill_prob_milli.clamp(1, 1000)).unwrap_or(1000);
        size.saturating_mul(1000).div_ceil(p)
    }

    fn init(&self, side: Side, p: Price, last: Option<Quote>) -> Init {
        let Some(q) = last else {
            return Init::Resting(None);
        };
        let s = sign(side);
        let (best, best_size, opp) = sides(side, &q);
        if (opp - p).saturating_mul(s) <= 0 {
            return Init::Marketable(opp);
        }
        match (p - best).saturating_mul(s) {
            d if d > 0 => Init::Resting(Some(0)),
            0 => Init::Resting(Some(self.inflate(best_size))),
            _ => Init::Resting(None),
        }
    }

    fn match_limit(
        &self,
        book: &mut BookState,
        ev: &MarketEvent,
        o: &WorkingOrder,
        p: Price,
    ) -> Option<Trigger> {
        let fill = |price: Price| Trigger {
            order_id: o.id,
            price,
            reference: price,
            at_open: false,
        };
        let mut st = match book.queues.get(&o.id) {
            Some(st) if st.price == p && st.side == o.side => *st,
            _ => match self.init(o.side, p, book.quotes.get(&ev.instrument).copied()) {
                Init::Marketable(px) => {
                    book.queues.remove(&o.id);
                    return Some(fill(px));
                }
                Init::Resting(ahead) => QueueState {
                    side: o.side,
                    price: p,
                    ahead,
                },
            },
        };
        let s = sign(o.side);
        let filled = match &ev.payload {
            MarketPayload::Trade(t) => {
                let d = (t.price - p).saturating_mul(s);
                if d < 0 {
                    // Fiyat limitin içinden geçti.
                    true
                } else if d == 0 && t.size > 0 && t.aggressor != Some(o.side) {
                    match st.ahead {
                        Some(a) if t.size >= a => true,
                        Some(a) => {
                            st.ahead = Some(a - t.size);
                            false
                        }
                        // Kuyruk bilinmiyor: işlem kuyruğa sayılmaz (karamsar).
                        None => false,
                    }
                } else {
                    false
                }
            }
            MarketPayload::Quote(q) => {
                let (best, best_size, opp) = sides(o.side, q);
                if (opp - p).saturating_mul(s) < 0 {
                    // Karşı taraf limitin içine girdi.
                    true
                } else {
                    match (p - best).saturating_mul(s) {
                        // Seviye en iyi: görünür boyut önümüzdeki kuyruğun üst sınırı.
                        0 => {
                            let bound = self.inflate(best_size);
                            st.ahead = Some(st.ahead.map_or(bound, |a| a.min(bound)));
                        }
                        // Seviye kayboldu, biz en iyideyiz: önümüzde kimse yok.
                        d if d > 0 => st.ahead = Some(0),
                        _ => {}
                    }
                    false
                }
            }
            MarketPayload::Bar(_) | MarketPayload::Feed(_) => false,
        };
        if filled {
            book.queues.remove(&o.id);
            Some(fill(p))
        } else {
            book.queues.insert(o.id, st);
            None
        }
    }
}

impl FillModel for MboQueueFill {
    fn name(&self) -> &'static str {
        QUEUE_MODEL_NAME
    }
    fn version(&self) -> u32 {
        self.cfg.version
    }
    fn handles(&self, payload: &MarketPayload) -> bool {
        matches!(payload, MarketPayload::Trade(_) | MarketPayload::Quote(_))
    }

    fn match_one(
        &self,
        ev: &MarketEvent,
        o: &WorkingOrder,
        slippage_ticks: i64,
        _scope: BarScope,
    ) -> Option<Trigger> {
        if o.active_from > ev.ts {
            return None;
        }
        // Durum ödünç alınamıyorsa (yeniden giriş) dolum yok: karamsar, panik yok.
        let mut book = self.state.try_borrow_mut().ok()?;
        if let MarketPayload::Quote(q) = &ev.payload {
            book.quotes.insert(ev.instrument, *q);
        }
        let aggressive = |x: Price| {
            let ticks = slippage_ticks.saturating_add(self.impact_ticks(o.qty));
            Trigger {
                order_id: o.id,
                price: x + ticks.saturating_mul(sign(o.side)),
                reference: x,
                at_open: false,
            }
        };
        match (o.kind, &ev.payload) {
            (OrderKind::Market, MarketPayload::Trade(t)) => Some(aggressive(t.price)),
            (OrderKind::Market, MarketPayload::Quote(q)) => Some(aggressive(sides(o.side, q).2)),
            (OrderKind::Market, _) => None,
            (OrderKind::Stop(p), MarketPayload::Trade(t)) => {
                ((t.price - p).saturating_mul(sign(o.side)) >= 0).then(|| aggressive(t.price))
            }
            (OrderKind::Stop(_), _) => None,
            (OrderKind::Limit(p), _) => self.match_limit(&mut book, ev, o, p),
        }
    }

    fn observe(&self, ev: &MarketEvent) {
        if let MarketPayload::Quote(q) = &ev.payload
            && let Ok(mut book) = self.state.try_borrow_mut()
        {
            book.quotes.insert(ev.instrument, *q);
        }
    }

    fn order_closed(&self, order_id: u64) {
        if let Ok(mut book) = self.state.try_borrow_mut() {
            book.queues.remove(&order_id);
        }
    }
}

/// Alış +1, satış −1 (tick cinsinden "aleyhte" yönü).
fn sign(side: Side) -> i64 {
    i64::from(side.sign())
}

/// (kendi tarafın en iyisi, görünür boyutu, karşı tarafın en iyisi).
fn sides(side: Side, q: &Quote) -> (Price, u64, Price) {
    match side {
        Side::Buy => (q.bid, q.bid_size, q.ask),
        Side::Sell => (q.ask, q.ask_size, q.bid),
    }
}

/// `ceil(milli × (adet − 1) / 1000)`, negatif parametre 0 sayılır.
pub(crate) fn impact_ticks(milli_per_lot: i64, qty: i32) -> i64 {
    let extra = i64::from(qty.saturating_sub(1)).max(0);
    let m = milli_per_lot.max(0).saturating_mul(extra);
    // m ≥ 0 olduğundan yukarı yuvarlama güvenli.
    m.saturating_add(999) / 1000
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fill::{Role, TradeFill};
    use atlas_core::{StrategyId, Trade, UnixNanos};

    const I: InstrumentId = InstrumentId(0);

    fn quote(ts: u64, bid: i64, bid_size: u64, ask: i64, ask_size: u64) -> MarketEvent {
        MarketEvent {
            ts: UnixNanos(ts),
            instrument: I,
            payload: MarketPayload::Quote(Quote {
                bid: Price(bid),
                ask: Price(ask),
                bid_size,
                ask_size,
            }),
        }
    }

    fn trade(ts: u64, px: i64, size: u64, aggressor: Option<Side>) -> MarketEvent {
        MarketEvent::trade(
            I,
            UnixNanos(ts),
            Trade {
                price: Price(px),
                size,
                aggressor,
            },
        )
    }

    fn order(id: u64, side: Side, qty: i32, kind: OrderKind) -> WorkingOrder {
        WorkingOrder {
            id,
            strategy: StrategyId::new("s"),
            instrument: I,
            side,
            qty,
            kind,
            role: Role::Entry,
            active_from: UnixNanos(0),
            created_seq: 0,
            expiry: None,
        }
    }

    fn m1(m: &MboQueueFill, ev: &MarketEvent, o: &WorkingOrder) -> Option<Trigger> {
        m.observe(ev);
        m.match_one(ev, o, 1, BarScope::Full)
    }

    /// (a) Önünde 10 lot: o fiyatta 9 lot işlem → dolum yok; 1 lot daha → dolum.
    #[test]
    fn queue_ahead_must_be_exhausted() {
        let m = MboQueueFill::default();
        let o = order(1, Side::Buy, 1, OrderKind::Limit(Price(100)));
        assert!(m1(&m, &quote(1, 100, 10, 101, 5), &o).is_none());
        assert_eq!(m.queue_ahead(1), Some(Some(10)));
        assert!(m1(&m, &trade(2, 100, 9, Some(Side::Sell)), &o).is_none());
        assert_eq!(m.queue_ahead(1), Some(Some(1)));
        // Karşı taraftaki (alıcı agresör) işlem kuyruğumuzu tüketmez.
        assert!(m1(&m, &trade(3, 100, 4, Some(Side::Buy)), &o).is_none());
        let t = m1(&m, &trade(4, 100, 1, Some(Side::Sell)), &o).expect("dolum");
        assert_eq!(t.price, Price(100));
        assert_eq!(t.reference, Price(100));
        assert_eq!(m.queue_ahead(1), None, "dolan emrin durumu bırakılır");
    }

    /// (b) Fiyat limitin içinden geçerse kuyruk ne olursa olsun dolum.
    #[test]
    fn trade_through_fills_regardless_of_queue() {
        let m = MboQueueFill::default();
        let o = order(1, Side::Buy, 1, OrderKind::Limit(Price(100)));
        assert!(m1(&m, &quote(1, 100, 500, 101, 5), &o).is_none());
        let t = m1(&m, &trade(2, 99, 1, None), &o).expect("içinden geçiş");
        assert_eq!(t.price, Price(100));
        // Satış tarafı: kotasyonda karşı taraf limitin içine girerse de dolar.
        let s = order(2, Side::Sell, 1, OrderKind::Limit(Price(110)));
        assert!(m1(&m, &quote(3, 109, 5, 110, 300), &s).is_none());
        assert!(m1(&m, &quote(4, 111, 5, 112, 3), &s).is_some());
    }

    /// (c) Daha iyi fiyattaki limit: kuyruk 0, ilk işlemde dolar.
    #[test]
    fn better_than_best_has_empty_queue() {
        let m = MboQueueFill::default();
        m.observe(&quote(1, 100, 50, 104, 50));
        let o = order(1, Side::Buy, 1, OrderKind::Limit(Price(102)));
        assert!(m1(&m, &trade(2, 104, 3, Some(Side::Buy)), &o).is_none());
        assert_eq!(m.queue_ahead(1), Some(Some(0)));
        assert!(m1(&m, &trade(3, 102, 1, Some(Side::Sell)), &o).is_some());
    }

    #[test]
    fn behind_best_is_unknown_until_level_becomes_best() {
        let m = MboQueueFill::default();
        m.observe(&quote(1, 100, 50, 101, 50));
        let o = order(1, Side::Buy, 1, OrderKind::Limit(Price(98)));
        // Kitabın içinde: kuyruk bilinmez, o fiyattaki işlem sayılmaz.
        assert!(m1(&m, &trade(2, 98, 1000, Some(Side::Sell)), &o).is_none());
        assert_eq!(m.queue_ahead(1), Some(None));
        // Seviye en iyi oldu: görünür boyut üst sınır.
        assert!(m1(&m, &quote(3, 98, 7, 99, 4), &o).is_none());
        assert_eq!(m.queue_ahead(1), Some(Some(7)));
        // İptallerle boyut düştü → tahmin küçülür.
        assert!(m1(&m, &quote(4, 98, 3, 99, 4), &o).is_none());
        assert_eq!(m.queue_ahead(1), Some(Some(3)));
        // Seviye kayboldu (bid 97): önümüzde kimse yok.
        assert!(m1(&m, &quote(5, 97, 9, 99, 4), &o).is_none());
        assert_eq!(m.queue_ahead(1), Some(Some(0)));
    }

    #[test]
    fn marketable_limit_fills_at_touch_and_latency_respected() {
        let m = MboQueueFill::default();
        m.observe(&quote(1, 100, 5, 101, 5));
        let o = order(1, Side::Buy, 1, OrderKind::Limit(Price(103)));
        let t = m1(&m, &trade(2, 101, 1, None), &o).expect("pazarlanabilir");
        assert_eq!(t.price, Price(101));
        let mut late = order(2, Side::Buy, 1, OrderKind::Market);
        late.active_from = UnixNanos(10);
        assert!(m1(&m, &trade(5, 101, 1, None), &late).is_none());
        assert!(m1(&m, &trade(10, 101, 1, None), &late).is_some());
    }

    /// (d) Piyasa etkisi adetle artar.
    #[test]
    fn market_impact_grows_with_qty() {
        let m = MboQueueFill::new(QueueConfig {
            impact_milli_ticks_per_lot: 500,
            ..QueueConfig::default()
        });
        let ev = trade(1, 1000, 1, None);
        let px = |qty: i32| {
            let o = order(
                u64::try_from(qty).unwrap_or(0),
                Side::Buy,
                qty,
                OrderKind::Market,
            );
            m.match_one(&ev, &o, 1, BarScope::Full)
                .expect("piyasa")
                .price
        };
        assert_eq!(px(1), Price(1001)); // yalnız kayma
        assert_eq!(px(2), Price(1002)); // ceil(0,5) = 1
        assert_eq!(px(5), Price(1003)); // 1 + 2
        assert_eq!(px(10), Price(1006)); // 1 + ceil(4,5)
        assert!(px(1) < px(2) && px(2) < px(5) && px(5) < px(10));
        // Satışta aleyhte = aşağı; stop tetik fiyatı da etkiden geçer.
        let st = order(99, Side::Sell, 5, OrderKind::Stop(Price(1000)));
        let t = m.match_one(&ev, &st, 1, BarScope::Full).expect("stop");
        assert_eq!(t.price, Price(997));
        assert_eq!(t.reference, Price(1000));
    }

    #[test]
    fn fill_prob_inflates_visible_queue() {
        let m = MboQueueFill::new(QueueConfig {
            limit_fill_prob_milli: 500,
            ..QueueConfig::default()
        });
        let o = order(1, Side::Sell, 1, OrderKind::Limit(Price(101)));
        assert!(m1(&m, &quote(1, 100, 1, 101, 10), &o).is_none());
        assert_eq!(m.queue_ahead(1), Some(Some(20)));
    }

    #[test]
    fn same_market_rules_as_trade_fill_for_one_lot_without_impact() {
        let q = MboQueueFill::default();
        let tf = TradeFill::default();
        let ev = trade(1, 100, 1, None);
        for o in [
            order(1, Side::Buy, 1, OrderKind::Market),
            order(2, Side::Sell, 1, OrderKind::Stop(Price(101))),
            order(3, Side::Buy, 1, OrderKind::Stop(Price(100))),
        ] {
            assert_eq!(
                q.match_one(&ev, &o, 2, BarScope::Full),
                tf.match_one(&ev, &o, 2, BarScope::Full)
            );
        }
        assert_eq!(q.name(), "queue");
        assert_eq!(
            MboQueueFill::from_params(&FillParams::default()).version(),
            1
        );
    }

    /// Motorla uçtan uca (motor değişmeden): kotasyonsuz akışta kuyruk hep "bilinmiyor" olduğundan
    /// limitler yalnız içinden geçişle dolar → `TradeFill` (TradeThrough) ile birebir aynı işlemler.
    /// Kotasyonlu akışta koşu hatasız ve deterministik.
    #[test]
    fn engine_end_to_end() {
        use crate::engine::SimConfig;
        use crate::synthetic::random_walk_trades;
        use crate::validation::{RandomEntry, run_sim, validation_env};
        use atlas_core::Strategy;
        use atlas_core::units::{NANOS_PER_MIN, NANOS_PER_SEC};

        let env = validation_env().expect("ortam");
        let evs = random_walk_trades(
            7,
            I,
            UnixNanos::from_secs(1_700_000_000),
            100_000,
            NANOS_PER_SEC,
            Price(100_000),
        );
        let run = |m: &dyn FillModel, evs: &[MarketEvent]| {
            let mut st: Vec<Box<dyn Strategy>> = vec![Box::new(RandomEntry::new(
                7,
                I,
                0.05,
                10,
                10,
                30 * NANOS_PER_MIN,
            ))];
            run_sim(&env, m, SimConfig::default(), evs.to_vec(), &mut st).expect("koşu")
        };
        let q = run(&MboQueueFill::default(), &evs);
        let t = run(&TradeFill::default(), &evs);
        assert!(!q.trades.is_empty());
        assert_eq!(q.trades, t.trades);
        assert_eq!(q.fill_model, "queue@v1");

        let mut with_quotes = Vec::new();
        for e in &evs {
            if let MarketPayload::Trade(tr) = &e.payload {
                with_quotes.push(quote(e.ts.0, tr.price.0 - 1, 25, tr.price.0 + 1, 25));
            }
            with_quotes.push(e.clone());
        }
        let a = run(&MboQueueFill::default(), &with_quotes);
        let b = run(&MboQueueFill::default(), &with_quotes);
        assert!(!a.trades.is_empty());
        assert_eq!(a, b);
    }
}
