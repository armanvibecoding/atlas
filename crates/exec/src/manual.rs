//! Operatör (Grok) sanal hesabı — ADR 0010.
//!
//! Grok'un elle verdiği emirler ayrı bir `Engine` hesabında, sistemin stratejileriyle AYNI kod yolundan
//! (RiskGate + BarFill, kural 3) işlenir. Emirler `ManualStrategy` aracılığıyla `Intent` olur; boyutu Grok
//! seçer ama stop zorunludur ve işlem başı risk, kontrat tavanı, günlük zarar, taban, consistency
//! kontrolleri prop risk kapısında aynen uygulanır.
//!
//! Belirlenimcilik: her emir journal'da hangi barın kapanışında uygulandığıyla (`grok_order_applied`)
//! kayıtlıdır. Daemon yeniden başladığında hesap, depodaki barlar + bu kayıtlar yeniden oynatılarak bit
//! düzeyinde aynı kurulur.

use atlas_core::{
    Action, Ctx, EntryType, Event, EventKind, InstrumentId, Intent, MarketEvent, MarketPayload,
    Price, Side, Strategy, StrategyId, UnixNanos,
};
use atlas_risk::RiskGate;
use atlas_sim::{BarFill, Engine, SimConfig, TradeRecord};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Grok hesabının strateji kimliği (kill kapsamı `strategy:grok`).
pub const GROK_ID: &str = "grok";

/// Operatör emri (journal `grok_order` yükü).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ManualOrder {
    Open {
        side: Side,
        /// Pozitif adet (lot; BTCUSDT'de 1 = 0.001 BTC).
        qty: i32,
        /// `None` = piyasa (sonraki barın açılışı).
        limit: Option<Price>,
        stop: Price,
        target: Option<Price>,
        /// Limit emrin geçerlilik süresi (dakika).
        expiry_min: Option<u32>,
        reason: String,
    },
    Close {
        reason: String,
    },
    Cancel {
        reason: String,
    },
    /// Hesabın kill durumunu bu bara eşitle (etiket → sebep; `global` / `account`). Daemon, CLI kill/kill-reset
    /// değişikliklerini bu kayıtla uygular ki yeniden kurulum aynı anda aynı kill durumunu görsün.
    SyncKills {
        kills: std::collections::BTreeMap<String, String>,
    },
}

impl ManualOrder {
    pub fn is_open(&self) -> bool {
        matches!(self, ManualOrder::Open { .. })
    }
}

/// Uygulanacak emir: journal'daki istek numarası + emir.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedOrder {
    pub request_seq: i64,
    pub order: ManualOrder,
}

type Queue = Arc<Mutex<VecDeque<QueuedOrder>>>;

/// Kuyruktaki emirleri bar olayında `Intent`'e çevirir.
struct ManualStrategy {
    id: StrategyId,
    inst: InstrumentId,
    queue: Queue,
}

impl Strategy for ManualStrategy {
    fn id(&self) -> &StrategyId {
        &self.id
    }

    fn on_event(&mut self, _ctx: &Ctx<'_>, ev: &MarketEvent) -> Vec<Intent> {
        if ev.instrument != self.inst || !matches!(ev.payload, MarketPayload::Bar(_)) {
            return Vec::new();
        }
        let Ok(mut q) = self.queue.lock() else {
            return Vec::new();
        };
        q.drain(..)
            .filter_map(|o| {
                let action = match o.order {
                    ManualOrder::Open {
                        side,
                        qty,
                        limit,
                        stop,
                        target,
                        expiry_min,
                        ..
                    } => Action::Open {
                        side,
                        qty,
                        entry: limit.map_or(EntryType::Market, EntryType::Limit),
                        stop_loss: stop,
                        take_profit: target,
                        time_exit: None,
                        entry_expiry: limit.map(|_| {
                            ev.ts.plus(
                                u64::from(expiry_min.unwrap_or(60).max(1))
                                    * atlas_core::units::NANOS_PER_MIN,
                            )
                        }),
                    },
                    ManualOrder::Close { reason } => Action::Close { reason },
                    ManualOrder::Cancel { .. } => Action::CancelEntry,
                    // Kill eşitlemesi strateji niyeti değildir; hesap adımından önce uygulanır.
                    ManualOrder::SyncKills { .. } => return None,
                };
                Some(Intent {
                    strategy: self.id.clone(),
                    instrument: self.inst,
                    ts: ev.ts,
                    action,
                })
            })
            .collect()
    }
}

/// Emir durumu (anlık görüntü için).
#[derive(Debug, Clone, Serialize)]
pub struct OrderStatus {
    pub request_seq: i64,
    pub bar_ts: UnixNanos,
    pub order: ManualOrder,
    /// applied | rejected | filled
    pub status: String,
    pub detail: Value,
}

/// Grok'un sanal hesabı.
pub struct ManualAccount {
    pub inst: atlas_core::Instrument,
    engine: Engine,
    strategies: Vec<Box<dyn Strategy>>,
    queue: Queue,
    orders: Vec<OrderStatus>,
    pub started: UnixNanos,
}

impl ManualAccount {
    pub fn new(inst: &atlas_core::Instrument, gate: Arc<RiskGate>, start: UnixNanos) -> Self {
        let queue: Queue = Arc::new(Mutex::new(VecDeque::new()));
        let cfg = SimConfig {
            max_bars_history: 64,
            ..SimConfig::default()
        };
        ManualAccount {
            inst: inst.clone(),
            engine: Engine::new(gate, Arc::new(BarFill::default()), cfg, start),
            strategies: vec![Box::new(ManualStrategy {
                id: StrategyId::new(GROK_ID),
                inst: inst.id,
                queue: Arc::clone(&queue),
            })],
            queue,
            orders: Vec::new(),
            started: start,
        }
    }

    /// Etkin kill kapsamları (etiket → sebep).
    pub fn kills(&self) -> std::collections::BTreeMap<String, String> {
        self.engine
            .state()
            .kills
            .active()
            .into_iter()
            .map(|(s, r)| (s.label(), r.reason))
            .collect()
    }

    /// Kill durumunu `wanted`'a eşitler (eksikleri tetikler, fazlaları kaldırır).
    pub fn apply_kills(
        &mut self,
        wanted: &std::collections::BTreeMap<String, String>,
        ts: UnixNanos,
    ) {
        let ks = &mut self.engine.state_mut().kills;
        let active: Vec<String> = ks.active().into_iter().map(|(s, _)| s.label()).collect();
        for (label, reason) in wanted {
            if !active.contains(label)
                && let Ok(sc) = atlas_risk::KillScope::parse(label)
            {
                ks.trigger(sc, reason, ts);
            }
        }
        for a in active {
            if !wanted.contains_key(&a)
                && let Ok(sc) = atlas_risk::KillScope::parse(&a)
            {
                ks.reset(&sc, ts);
            }
        }
    }

    /// Hesabın işlediği son olay zamanı (bundan eski barlar yok sayılır).
    pub fn last_ts(&self) -> UnixNanos {
        self.engine.last_ts()
    }

    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    pub fn trades(&self) -> &[TradeRecord] {
        self.engine.trades()
    }

    /// Bir piyasa olayını işler; `orders` bu barın kapanışında uygulanır. Üretilen olaylar döner.
    pub fn step(
        &mut self,
        ev: &MarketEvent,
        orders: Vec<QueuedOrder>,
    ) -> Result<Vec<Event>, atlas_sim::SimError> {
        if ev.ts < self.engine.last_ts() {
            return Ok(Vec::new());
        }
        let first_new = self.orders.len();
        for o in &orders {
            self.orders.push(OrderStatus {
                request_seq: o.request_seq,
                bar_ts: ev.ts,
                order: o.order.clone(),
                status: "applied".into(),
                detail: Value::Null,
            });
        }
        // Kill eşitlemeleri barın işlenmesinden ÖNCE (canlıdaki sıra: senkron → adım).
        for o in &orders {
            if let ManualOrder::SyncKills { kills } = &o.order {
                self.apply_kills(kills, ev.ts);
            }
        }
        if let Ok(mut q) = self.queue.lock() {
            q.extend(
                orders
                    .into_iter()
                    .filter(|o| !matches!(o.order, ManualOrder::SyncKills { .. })),
            );
        }
        self.engine.step(ev, &mut self.strategies)?;
        let events = self.engine.drain_events();
        // Red ve dolumları bu adımın açılış emirlerine sırayla eşle (kapatma/iptal reddedilmez).
        let mut open_idx = (first_new..self.orders.len())
            .filter(|i| self.orders[*i].order.is_open())
            .collect::<VecDeque<_>>();
        for e in &events {
            let ours = e.payload.get("strategy").and_then(Value::as_str) == Some(GROK_ID);
            if !ours {
                continue;
            }
            match e.kind {
                EventKind::IntentRejected => {
                    if let Some(i) = open_idx.pop_front() {
                        self.orders[i].status = "rejected".into();
                        self.orders[i].detail =
                            e.payload.get("reason").cloned().unwrap_or_default();
                    }
                }
                EventKind::PositionOpened => {
                    // Limit emir sonraki barlarda dolabilir: en son "applied" açılış emri.
                    if let Some(o) = self
                        .orders
                        .iter_mut()
                        .rev()
                        .find(|o| o.order.is_open() && o.status == "applied")
                    {
                        o.status = "filled".into();
                        o.detail = json!({"price": e.payload.get("price"), "ts": e.ts});
                    }
                }
                _ => {}
            }
        }
        Ok(events)
    }

    /// Hesap anlık görüntüsü (CLI `trade list` okur). Fiyatlar ondalık metin, para USD.
    pub fn snapshot(&self, now: UnixNanos) -> Value {
        let gate_env = self.engine.gate_env();
        let st = self.engine.state();
        let id = StrategyId::new(GROK_ID);
        let px = |p: Price| self.inst.price_to_fixed9(p).to_string();
        let size = |q: i32| {
            atlas_core::units::Fixed9(i64::from(q).saturating_mul(self.inst.lot_size.0)).to_string()
        };
        let last = st.last_price(self.inst.id);
        let position = st.position_of(&id, self.inst.id).map(|p| {
            let unreal = last.map(|l| {
                self.inst
                    .pnl(p.entry, l, atlas_core::Qty(p.signed()))
                    .as_usd_f64()
            });
            let risk = match p.side {
                Side::Buy => p.entry - p.stop,
                Side::Sell => p.stop - p.entry,
            };
            json!({"side": p.side, "size": size(p.qty), "qty": p.qty, "entry": px(p.entry), "stop": px(p.stop),
                "opened": p.opened, "unrealized_usd": unreal,
                "risk_to_stop_usd": (self.inst.tick_value * risk.max(0).saturating_mul(i64::from(p.qty))).as_usd_f64()})
        });
        let pending = st.pending().get(&(id.clone(), self.inst.id)).map(|p| {
            json!({"side": p.side, "size": size(p.qty), "entry_ref": px(p.entry_ref), "stop": px(p.stop)})
        });
        let trades: Vec<Value> = self
            .engine
            .trades()
            .iter()
            .rev()
            .take(50)
            .map(|t| {
                json!({"side": t.side, "size": size(t.qty), "entry_ts": t.entry_ts, "entry": px(t.entry_price),
                    "exit_ts": t.exit_ts, "exit": px(t.exit_price), "exit_reason": t.exit_reason,
                    "net_usd": t.net.as_usd_f64(), "fees_usd": t.fees.as_usd_f64()})
            })
            .collect();
        let all = self.engine.trades();
        let wins = all.iter().filter(|t| t.net.0 > 0).count();
        let net: f64 = all.iter().map(|t| t.net.as_usd_f64()).sum();
        let kills: Vec<Value> = st
            .kills
            .active()
            .into_iter()
            .map(|(s, r)| json!({"scope": s.label(), "reason": r.reason, "ts": r.ts}))
            .collect();
        // Emirler: fiyatlar ondalık, boyut dayanak cinsinden (Grok'un okuyacağı biçim).
        let orders: Vec<Value> = self
            .orders
            .iter()
            .rev()
            .take(30)
            .map(|o| {
                let order = match &o.order {
                    ManualOrder::Open {
                        side,
                        qty,
                        limit,
                        stop,
                        target,
                        expiry_min,
                        reason,
                    } => json!({"op": "open", "side": side, "size": size(*qty), "limit": limit.map(px),
                        "stop": px(*stop), "target": target.map(px), "expiry_min": expiry_min, "reason": reason}),
                    ManualOrder::Close { reason } => json!({"op": "close", "reason": reason}),
                    ManualOrder::Cancel { reason } => json!({"op": "cancel", "reason": reason}),
                    ManualOrder::SyncKills { kills } => json!({"op": "sync_kills", "kills": kills}),
                };
                let mut detail = o.detail.clone();
                if let Some(p) = detail.get("price").and_then(Value::as_i64) {
                    detail["price"] = json!(px(Price(p)));
                }
                json!({"order_id": o.request_seq, "bar_ts": o.bar_ts, "status": o.status,
                    "order": order, "detail": detail})
            })
            .collect();
        json!({
            "account": GROK_ID,
            "symbol": self.inst.symbol,
            "as_of": now,
            "started": self.started,
            "last_price": last.map(px),
            "balance_usd": st.balance.as_usd_f64(),
            "equity_usd": st.equity(gate_env).as_usd_f64(),
            "floor_usd": st.floor().as_usd_f64(),
            "day_start_equity_usd": st.day_start_equity().as_usd_f64(),
            "today_pnl_usd": st.today_profit(gate_env).as_usd_f64(),
            "limits": {
                "max_risk_per_trade_usd": gate_env.policy.max_risk_per_trade().as_usd_f64(),
                "daily_loss_limit_usd": gate_env.rules.daily_loss_limit.map(|m| m.as_usd_f64()),
                "drawdown_usd": gate_env.rules.drawdown_amount.as_usd_f64(),
                "lot_size": self.inst.lot_size.to_string(),
                "tick_size": self.inst.tick_size.to_string(),
            },
            "position": position,
            "pending_entry": pending,
            "kills": kills,
            "stats": {"trades": all.len(), "wins": wins, "net_usd": net},
            "trades": trades,
            "orders": orders,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::units::NANOS_PER_MIN;
    use atlas_core::{Bar, Price};

    fn bar(i: u64, o: i64, h: i64, l: i64, c: i64) -> MarketEvent {
        MarketEvent::bar(
            InstrumentId(0),
            Bar {
                open_ts: UnixNanos(i * NANOS_PER_MIN),
                close_ts: UnixNanos((i + 1) * NANOS_PER_MIN),
                open: Price(o),
                high: Price(h),
                low: Price(l),
                close: Price(c),
                volume: 10,
                buy_volume: 5,
                trades: 1,
            },
        )
    }

    fn account() -> ManualAccount {
        let env = atlas_sim::validation::validation_env().unwrap();
        let inst = env.instruments.by_id(InstrumentId(0)).unwrap().clone();
        ManualAccount::new(&inst, Arc::new(RiskGate::new(env)), UnixNanos(0))
    }

    fn open(seq: i64, side: Side, qty: i32, stop: i64, target: Option<i64>) -> QueuedOrder {
        QueuedOrder {
            request_seq: seq,
            order: ManualOrder::Open {
                side,
                qty,
                limit: None,
                stop: Price(stop),
                target: target.map(Price),
                expiry_min: None,
                reason: "test".into(),
            },
        }
    }

    #[test]
    fn piyasa_emri_sonraki_barda_dolar_ve_stopta_kapanir() {
        let mut a = account();
        a.step(&bar(0, 10_000, 10_010, 9_990, 10_000), vec![])
            .unwrap();
        a.step(
            &bar(1, 10_000, 10_010, 9_990, 10_000),
            vec![open(1, Side::Buy, 1, 9_900, None)],
        )
        .unwrap();
        let ev = a
            .step(&bar(2, 10_000, 10_020, 9_995, 10_010), vec![])
            .unwrap();
        assert!(
            ev.iter().any(|e| e.kind == EventKind::PositionOpened),
            "{ev:?}"
        );
        let snap = a.snapshot(UnixNanos(0));
        assert_eq!(snap["orders"][0]["status"], "filled");
        assert!(snap["position"].is_object());
        // Stop altına iniş → kapanır.
        let ev = a
            .step(&bar(3, 10_000, 10_000, 9_800, 9_850), vec![])
            .unwrap();
        assert!(
            ev.iter().any(|e| e.kind == EventKind::PositionClosed),
            "{ev:?}"
        );
        assert_eq!(a.trades().len(), 1);
        assert!(a.trades()[0].net.0 < 0);
    }

    #[test]
    fn risk_siniri_asan_emir_reddedilir() {
        let mut a = account();
        a.step(&bar(0, 10_000, 10_010, 9_990, 10_000), vec![])
            .unwrap();
        // Çok büyük adet → işlem başı risk ya da kontrat tavanı reddi.
        a.step(
            &bar(1, 10_000, 10_010, 9_990, 10_000),
            vec![open(7, Side::Buy, 1_000_000, 1, None)],
        )
        .unwrap();
        let snap = a.snapshot(UnixNanos(0));
        assert_eq!(snap["orders"][0]["status"], "rejected", "{snap}");
        assert!(snap["position"].is_null());
    }

    #[test]
    fn kapat_emri_pozisyonu_kapatir_ve_ayni_oynatma_ayni_sonuc() {
        let run = || {
            let mut a = account();
            a.step(&bar(0, 10_000, 10_010, 9_990, 10_000), vec![])
                .unwrap();
            a.step(
                &bar(1, 10_000, 10_010, 9_990, 10_000),
                vec![open(1, Side::Sell, 2, 10_200, Some(9_500))],
            )
            .unwrap();
            a.step(&bar(2, 10_000, 10_010, 9_950, 9_960), vec![])
                .unwrap();
            a.step(
                &bar(3, 9_960, 9_970, 9_940, 9_950),
                vec![QueuedOrder {
                    request_seq: 2,
                    order: ManualOrder::Close {
                        reason: "kâr al".into(),
                    },
                }],
            )
            .unwrap();
            a.step(&bar(4, 9_950, 9_960, 9_940, 9_945), vec![]).unwrap();
            a.trades().to_vec()
        };
        let t1 = run();
        assert_eq!(t1.len(), 1);
        assert!(t1[0].gross.0 > 0, "{t1:?}");
        assert_eq!(t1, run(), "aynı emir+bar dizisi → aynı sonuç");
    }
}
