#![allow(clippy::unwrap_used)]

//! Sandbox kabul testleri (M4: "plugin sandbox'ı ağ/dosya/saat erişimini reddeder").
//!
//! Modüller WAT metniyle yazılır; wasm32 hedefi gerekmez. Örnek plugin testleri derlenmiş
//! `fixtures/example_momentum.wasm`'ı kullanır (yeniden üretim: `plugins/example_momentum/README.md`).

use atlas_core::{
    AccountView, Action, Bar, Ctx, EntryType, InstrumentId, Intent, MarketEvent, MarketHistory,
    Money, PositionView, Price, SessionView, Side, Strategy, StrategyId, UnixNanos,
};
use atlas_plugin::{
    CallError, PluginError, PluginLimits, PluginModule, PluginRunner, PluginStrategy,
};
use std::time::{Duration, Instant};

const INST: InstrumentId = InstrumentId(3);
const BAR_NS: u64 = 60_000_000_000;

// ---------- yardımcılar ----------

struct Acct {
    position: Option<PositionView>,
    pending: bool,
}
impl AccountView for Acct {
    fn position(&self, _: &StrategyId, _: InstrumentId) -> Option<PositionView> {
        self.position
    }
    fn has_pending_entry(&self, _: &StrategyId, _: InstrumentId) -> bool {
        self.pending
    }
    fn equity(&self) -> Money {
        Money(0)
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

fn bar(k: u64, close: i64) -> Bar {
    Bar {
        open_ts: UnixNanos(k * BAR_NS),
        close_ts: UnixNanos((k + 1) * BAR_NS),
        open: Price(close),
        high: Price(close + 2),
        low: Price(close - 2),
        close: Price(close),
        volume: 100,
        buy_volume: 50,
        trades: 10,
    }
}

/// Barları sırayla besler; her bar olayının intent'lerini döndürür.
fn drive(s: &mut PluginStrategy, closes: &[i64], acct: &Acct) -> Vec<Vec<Intent>> {
    let mut h = MarketHistory::new(0);
    let sid = s.id().clone();
    closes
        .iter()
        .enumerate()
        .map(|(k, &c)| {
            let ev = MarketEvent::bar(INST, bar(k as u64, c));
            h.push(&ev);
            let ctx = Ctx::new(&sid, &h, acct, &Sess);
            s.on_event(&ctx, &ev)
        })
        .collect()
}

fn flat() -> Acct {
    Acct {
        position: None,
        pending: false,
    }
}

/// Sabit bir çıktı döndüren modül (çıktı veri segmentinde, 1024 adresinde).
fn fixed_output_module(out: &str) -> String {
    let esc = out.replace('\\', "\\\\").replace('"', "\\\"");
    let ret = (1024u64 << 32) | out.len() as u64;
    format!(
        r#"(module
            (memory (export "memory") 1)
            (data (i32.const 1024) "{esc}")
            (func (export "alloc") (param i32) (result i32) i32.const 8192)
            (func (export "on_event") (param i32 i32) (result i64) i64.const {ret}))"#
    )
}

fn load(wat: &str) -> PluginModule {
    PluginModule::from_bytes(wat.as_bytes()).unwrap()
}

fn strategy(wat: &str, limits: PluginLimits) -> PluginStrategy {
    PluginStrategy::new("t", load(wat), INST, limits).unwrap()
}

const ABI_TAIL: &str = r#"
    (memory (export "memory") 1)
    (func (export "alloc") (param i32) (result i32) i32.const 0)
    (func (export "on_event") (param i32 i32) (result i64) i64.const 0)"#;

// ---------- ağ / dosya / saat reddi ----------

#[test]
fn wasi_and_any_import_is_rejected() {
    let cases = [
        // dosya / stdout
        (
            "wasi_snapshot_preview1",
            "fd_write",
            "(param i32 i32 i32 i32) (result i32)",
        ),
        (
            "wasi_snapshot_preview1",
            "path_open",
            "(param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)",
        ),
        // ağ
        (
            "wasi_snapshot_preview1",
            "sock_open",
            "(param i32 i32 i32) (result i32)",
        ),
        (
            "wasi_snapshot_preview1",
            "sock_send",
            "(param i32 i32 i32 i32 i32) (result i32)",
        ),
        (
            "wasi_snapshot_preview1",
            "sock_recv",
            "(param i32 i32 i32 i32 i32 i32) (result i32)",
        ),
        (
            "wasi_snapshot_preview1",
            "sock_accept",
            "(param i32 i32 i32) (result i32)",
        ),
        // saat ve rastgelelik
        (
            "wasi_snapshot_preview1",
            "clock_time_get",
            "(param i32 i64 i32) (result i32)",
        ),
        (
            "wasi_snapshot_preview1",
            "random_get",
            "(param i32 i32) (result i32)",
        ),
        // uydurma host fonksiyonu
        ("env", "now", "(result i64)"),
    ];
    for (m, n, sig) in cases {
        let wat = format!(r#"(module (import "{m}" "{n}" (func {sig})) {ABI_TAIL})"#);
        match PluginModule::from_bytes(wat.as_bytes()) {
            Err(PluginError::ForbiddenImport { module, name }) => {
                assert_eq!((module.as_str(), name.as_str()), (m, n));
            }
            other => panic!("{m}::{n} reddedilmeli, sonuç: {other:?}"),
        }
    }
    // Bellek/tablo/global importu da yasak (host hiçbir şey sağlamaz).
    let mem_import = r#"(module
        (import "env" "memory" (memory 1))
        (func (export "alloc") (param i32) (result i32) i32.const 0)
        (func (export "on_event") (param i32 i32) (result i64) i64.const 0))"#;
    assert!(matches!(
        PluginModule::from_bytes(mem_import.as_bytes()),
        Err(PluginError::ForbiddenImport { .. })
    ));
    let global_import = format!(r#"(module (import "env" "t0" (global i64)) {ABI_TAIL})"#);
    assert!(matches!(
        PluginModule::from_bytes(global_import.as_bytes()),
        Err(PluginError::ForbiddenImport { .. })
    ));
}

#[test]
fn missing_or_mistyped_exports_rejected() {
    let no_on_event = r#"(module (memory (export "memory") 1)
        (func (export "alloc") (param i32) (result i32) i32.const 0))"#;
    let wrong_sig = r#"(module (memory (export "memory") 1)
        (func (export "alloc") (param i32) (result i32) i32.const 0)
        (func (export "on_event") (param i32 i32) (result i32) i32.const 0))"#;
    let no_memory = r#"(module
        (func (export "alloc") (param i32) (result i32) i32.const 0)
        (func (export "on_event") (param i32 i32) (result i64) i64.const 0))"#;
    for wat in [no_on_event, wrong_sig, no_memory] {
        let r = PluginRunner::new(load(wat), PluginLimits::default());
        assert!(
            matches!(r, Err(PluginError::Abi(_))),
            "ABI hatası beklenirdi"
        );
    }
    assert!(matches!(
        PluginModule::from_bytes(b"(module (func"),
        Err(PluginError::Compile(_))
    ));
}

// ---------- yakıt ----------

#[test]
fn infinite_loop_stopped_by_fuel() {
    let wat = r#"(module (memory (export "memory") 1)
        (func (export "alloc") (param i32) (result i32) i32.const 0)
        (func (export "on_event") (param i32 i32) (result i64)
            (loop $l (br $l))
            i64.const 0))"#;
    let limits = PluginLimits {
        fuel_per_event: 2_000_000,
        ..PluginLimits::default()
    };
    let mut r = PluginRunner::new(load(wat), limits).unwrap();
    let t = Instant::now();
    assert_eq!(r.call(b"{}"), Err(CallError::OutOfFuel));
    // Tekrar çağrılabilir (temiz örnekle) ve yine durur.
    assert_eq!(r.call(b"{}"), Err(CallError::OutOfFuel));
    assert!(t.elapsed() < Duration::from_secs(10));
}

#[test]
fn infinite_loop_in_start_function_does_not_hang() {
    let wat = r#"(module (memory (export "memory") 1)
        (func $s (loop $l (br $l)))
        (start $s)
        (func (export "alloc") (param i32) (result i32) i32.const 0)
        (func (export "on_event") (param i32 i32) (result i64) i64.const 0))"#;
    let limits = PluginLimits {
        fuel_per_event: 1_000_000,
        ..PluginLimits::default()
    };
    assert!(matches!(
        PluginRunner::new(load(wat), limits),
        Err(PluginError::Instantiate(_))
    ));
}

#[test]
fn strategy_counts_fuel_exhaustion_and_disables_after_max_failures() {
    let wat = r#"(module (memory (export "memory") 1)
        (func (export "alloc") (param i32) (result i32) i32.const 0)
        (func (export "on_event") (param i32 i32) (result i64)
            (loop $l (br $l))
            i64.const 0))"#;
    let limits = PluginLimits {
        fuel_per_event: 200_000,
        max_failures: 3,
        ..PluginLimits::default()
    };
    let mut s = strategy(wat, limits);
    let out = drive(&mut s, &[100; 10], &flat());
    assert!(out.iter().all(Vec::is_empty));
    let st = s.stats();
    assert_eq!(st.out_of_fuel, 3);
    assert_eq!(st.calls, 3, "devre dışı kaldıktan sonra çağrılmamalı");
    assert!(st.disabled && s.is_disabled());
}

// ---------- bellek ----------

#[test]
fn initial_memory_over_limit_rejected() {
    // 1000 sayfa = 62,5 MiB > 16 MiB limit.
    let wat = r#"(module (memory (export "memory") 1000)
        (func (export "alloc") (param i32) (result i32) i32.const 0)
        (func (export "on_event") (param i32 i32) (result i64) i64.const 0))"#;
    let limits = PluginLimits {
        max_memory_bytes: 16 << 20,
        ..PluginLimits::default()
    };
    assert!(matches!(
        PluginRunner::new(load(wat), limits),
        Err(PluginError::Instantiate(_))
    ));
}

#[test]
fn memory_growth_over_limit_traps() {
    // Her çağrıda 100 sayfa (6,25 MiB) büyür; 16 MiB limitte üçüncü çağrı tuzağa düşer.
    let wat = r#"(module (memory (export "memory") 1)
        (func (export "alloc") (param i32) (result i32) i32.const 0)
        (func (export "on_event") (param i32 i32) (result i64)
            (drop (memory.grow (i32.const 100)))
            i64.const 0))"#;
    let limits = PluginLimits {
        max_memory_bytes: 16 << 20,
        ..PluginLimits::default()
    };
    let mut r = PluginRunner::new(load(wat), limits).unwrap();
    assert_eq!(r.call(b"{}"), Ok(vec![]));
    assert_eq!(r.call(b"{}"), Ok(vec![]));
    assert!(matches!(r.call(b"{}"), Err(CallError::Trap(_))));
    // Hata sonrası temiz örnek: bellek yine 1 sayfadan başlar.
    assert_eq!(r.call(b"{}"), Ok(vec![]));
}

// ---------- ABI ihlalleri ----------

#[test]
fn bad_pointers_and_oversized_output_are_abi_errors() {
    let bad_alloc = r#"(module (memory (export "memory") 1)
        (func (export "alloc") (param i32) (result i32) i32.const 65530)
        (func (export "on_event") (param i32 i32) (result i64) i64.const 0))"#;
    let mut r = PluginRunner::new(load(bad_alloc), PluginLimits::default()).unwrap();
    assert!(matches!(r.call(&[b' '; 100]), Err(CallError::Abi(_))));

    // Çıktı: ptr = 65000, len = 1000 → bellek dışı.
    let oob = format!(
        r#"(module (memory (export "memory") 1)
        (func (export "alloc") (param i32) (result i32) i32.const 0)
        (func (export "on_event") (param i32 i32) (result i64) i64.const {}))"#,
        (65_000u64 << 32) | 1_000
    );
    let mut r = PluginRunner::new(load(&oob), PluginLimits::default()).unwrap();
    assert!(matches!(r.call(b"{}"), Err(CallError::Abi(_))));

    let big = format!(
        r#"(module (memory (export "memory") 2)
        (func (export "alloc") (param i32) (result i32) i32.const 0)
        (func (export "on_event") (param i32 i32) (result i64) i64.const {}))"#,
        100_000u64
    );
    let limits = PluginLimits {
        max_output_bytes: 1_000,
        ..PluginLimits::default()
    };
    let mut r = PluginRunner::new(load(&big), limits).unwrap();
    assert!(matches!(r.call(b"{}"), Err(CallError::Abi(_))));
}

#[test]
fn unreachable_trap_counted() {
    let wat = r#"(module (memory (export "memory") 1)
        (func (export "alloc") (param i32) (result i32) i32.const 0)
        (func (export "on_event") (param i32 i32) (result i64) unreachable))"#;
    let mut s = strategy(wat, PluginLimits::default());
    let out = drive(&mut s, &[100, 101], &flat());
    assert!(out.iter().all(Vec::is_empty));
    assert_eq!(s.stats().traps, 2);
    assert!(!s.is_disabled());
}

// ---------- çıktı parse ----------

#[test]
fn valid_output_becomes_host_built_intents() {
    let json = r#"[{"action":"open","side":"buy","qty":2,"stop_loss":90,"take_profit":130,"time_exit_bars":5}]"#;
    let mut s = strategy(&fixed_output_module(json), PluginLimits::default());
    let out = drive(&mut s, &[100], &flat());
    let id = s.id().clone();
    assert!(id.as_str().starts_with("plugin:t#"));
    assert_eq!(
        out[0],
        vec![Intent {
            strategy: id,
            instrument: INST,
            ts: UnixNanos(BAR_NS),
            action: Action::Open {
                side: Side::Buy,
                qty: 2,
                entry: EntryType::Market,
                stop_loss: Price(90),
                take_profit: Some(Price(130)),
                time_exit: Some(UnixNanos(BAR_NS + 5 * BAR_NS)),
                entry_expiry: None,
            },
        }]
    );
    assert_eq!(s.stats().ok, 1);
    assert_eq!(s.stats().invalid_outputs, 0);

    // Close / cancel_entry de geçerli (pozisyon varken).
    let json = r#"[{"action":"close"},{"action":"cancel_entry"}]"#;
    let mut s = strategy(&fixed_output_module(json), PluginLimits::default());
    let acct = Acct {
        position: Some(PositionView {
            side: Side::Buy,
            qty: 1,
            entry: Price(95),
            stop: Price(90),
            opened: UnixNanos(0),
        }),
        pending: false,
    };
    let out = drive(&mut s, &[100], &acct);
    assert_eq!(out[0].len(), 2);
    assert!(matches!(out[0][0].action, Action::Close { .. }));
    assert_eq!(out[0][1].action, Action::CancelEntry);
}

#[test]
fn broken_or_invalid_output_dropped_and_counted() {
    let cases = [
        r#"[{"action":"open","side":"buy""#, // bozuk JSON
        r#"not json at all"#,                // JSON değil
        r#"[{"action":"open","side":"buy","qty":1,"stop_loss":90,"x":1}]"#, // bilinmeyen alan
        r#"[{"action":"transfer_funds","to":"x"}]"#, // bilinmeyen eylem
        r#"[{"action":"open","side":"buy","qty":1,"stop_loss":110}]"#, // stop yanlış tarafta
        r#"[{"action":"open","side":"buy","qty":-5,"stop_loss":90}]"#, // negatif adet
    ];
    for json in cases {
        let limits = PluginLimits {
            max_failures: 2,
            ..PluginLimits::default()
        };
        let mut s = strategy(&fixed_output_module(json), limits);
        let out = drive(&mut s, &[100, 100, 100, 100], &flat());
        assert!(out.iter().all(Vec::is_empty), "intent üretmemeli: {json}");
        let st = s.stats();
        assert_eq!(st.invalid_outputs, 2, "{json}");
        assert_eq!(st.calls, 2);
        assert!(st.disabled);
        assert!(st.last_error.is_some());
    }
}

#[test]
fn open_while_in_position_is_invalid() {
    let json = r#"[{"action":"open","side":"buy","qty":1,"stop_loss":90}]"#;
    let mut s = strategy(&fixed_output_module(json), PluginLimits::default());
    let acct = Acct {
        position: None,
        pending: true,
    };
    let out = drive(&mut s, &[100], &acct);
    assert!(out[0].is_empty());
    assert_eq!(s.stats().invalid_outputs, 1);
}

#[test]
fn only_own_bar_events_call_the_plugin() {
    let json = r#"[{"action":"open","side":"buy","qty":1,"stop_loss":90}]"#;
    let mut s = strategy(&fixed_output_module(json), PluginLimits::default());
    let mut h = MarketHistory::new(0);
    let sid = s.id().clone();
    let acct = flat();
    let other = MarketEvent::bar(InstrumentId(99), bar(0, 100));
    h.push(&other);
    let ctx = Ctx::new(&sid, &h, &acct, &Sess);
    assert!(s.on_event(&ctx, &other).is_empty());
    let trade = MarketEvent {
        ts: UnixNanos(BAR_NS),
        instrument: INST,
        payload: atlas_core::MarketPayload::Trade(atlas_core::Trade {
            price: Price(100),
            size: 1,
            aggressor: None,
        }),
    };
    h.push(&trade);
    let ctx = Ctx::new(&sid, &h, &acct, &Sess);
    assert!(s.on_event(&ctx, &trade).is_empty());
    assert_eq!(s.stats().calls, 0);
}

// ---------- determinizm ----------

/// Girdi baytlarının toplamı + `sqrt(-1)` (NaN) bitleri + `0/0` bitleri → 24 bayt çıktı.
const DETERMINISM_WAT: &str = r#"(module
    (memory (export "memory") 1)
    (func (export "alloc") (param i32) (result i32) i32.const 4096)
    (func (export "on_event") (param $p i32) (param $n i32) (result i64)
        (local $i i32) (local $acc i64)
        (block $done
            (loop $l
                (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
                (local.set $acc
                    (i64.add
                        (i64.mul (local.get $acc) (i64.const 31))
                        (i64.load8_u (i32.add (local.get $p) (local.get $i)))))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $l)))
        (i64.store (i32.const 0) (local.get $acc))
        (i64.store (i32.const 8)
            (i64.reinterpret_f64 (f64.sqrt (f64.convert_i64_s (i64.sub (i64.const 0) (i64.extend_i32_u (local.get $n)))))))
        (i64.store (i32.const 16)
            (i64.reinterpret_f64 (f64.div (f64.const 0) (f64.convert_i32_u (i32.sub (local.get $n) (local.get $n))))))
        i64.const 24))"#;

#[test]
fn same_input_same_output_and_canonical_nan() {
    let input = br#"{"abi":1,"instrument":3,"now":120,"bars":[]}"#;
    let run = || {
        let mut r = PluginRunner::new(load(DETERMINISM_WAT), PluginLimits::default()).unwrap();
        let a = r.call(input).unwrap();
        let fa = r.last_fuel_used();
        let b = r.call(input).unwrap();
        assert_eq!(fa, r.last_fuel_used(), "aynı girdi aynı yakıtı harcamalı");
        assert_eq!(a, b);
        (a, fa)
    };
    let (x, fx) = run();
    let (y, fy) = run();
    assert_eq!(x, y);
    assert_eq!(fx, fy);
    assert!(fx > 0);
    // NaN canonicalization: her iki NaN de kanonik sessiz NaN (0x7ff8...).
    let nan1 = u64::from_le_bytes(x[8..16].try_into().unwrap());
    let nan2 = u64::from_le_bytes(x[16..24].try_into().unwrap());
    assert_eq!(nan1, 0x7ff8_0000_0000_0000);
    assert_eq!(nan2, 0x7ff8_0000_0000_0000);
}

// ---------- örnek plugin (derlenmiş wasm) ----------

const EXAMPLE_WASM: &[u8] = include_bytes!("fixtures/example_momentum.wasm");

/// 30 bar yatay, sonra yukarı kırılım.
fn breakout_closes() -> Vec<i64> {
    let mut v = vec![1_000; 30];
    v.push(1_010);
    v
}

#[test]
fn example_plugin_loads_and_signals_breakout() {
    let module = PluginModule::from_bytes(EXAMPLE_WASM).unwrap();
    let mut s =
        PluginStrategy::new("example_momentum", module, INST, PluginLimits::default()).unwrap();
    let out = drive(&mut s, &breakout_closes(), &flat());
    assert!(
        out[..30].iter().all(Vec::is_empty),
        "yatayda sinyal olmamalı"
    );
    let last = &out[30];
    assert_eq!(last.len(), 1);
    // Kanalın alt ucu 998 (low = close - 2); risk 12, hedef 2R = 1034, zaman 60 bar.
    assert_eq!(
        last[0].action,
        Action::Open {
            side: Side::Buy,
            qty: 1,
            entry: EntryType::Market,
            stop_loss: Price(998),
            take_profit: Some(Price(1_034)),
            time_exit: Some(UnixNanos(31 * BAR_NS + 60 * BAR_NS)),
            entry_expiry: None,
        }
    );
    let st = s.stats();
    assert_eq!(st.calls, 31);
    assert_eq!(st.ok, 31);
    assert_eq!(st.failures(), 0);

    // Pozisyondayken sinyal yok.
    let module = PluginModule::from_bytes(EXAMPLE_WASM).unwrap();
    let mut s =
        PluginStrategy::new("example_momentum", module, INST, PluginLimits::default()).unwrap();
    let acct = Acct {
        position: None,
        pending: true,
    };
    assert!(
        drive(&mut s, &breakout_closes(), &acct)
            .iter()
            .all(Vec::is_empty)
    );
}

#[test]
fn example_plugin_is_deterministic() {
    // Sözde rastgele yürüyüş (sabit LCG; rastgelelik kaynağı yok).
    let mut x: u64 = 42;
    let closes: Vec<i64> = (0..400)
        .scan(10_000i64, |p, _| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *p += (x >> 61) as i64 - 3;
            Some(*p)
        })
        .collect();
    let run = || {
        let module = PluginModule::from_bytes(EXAMPLE_WASM).unwrap();
        let mut s =
            PluginStrategy::new("example_momentum", module, INST, PluginLimits::default()).unwrap();
        let out = drive(&mut s, &closes, &flat());
        assert_eq!(s.stats().failures(), 0);
        out
    };
    let a = run();
    let b = run();
    assert_eq!(a, b);
    assert!(
        a.iter().any(|v| !v.is_empty()),
        "test verisi en az bir sinyal üretmeli"
    );
}

/// Kaynaktan derler ve aynı senaryoyu koşar. `wasm32-unknown-unknown` hedefi ve ağ (crates.io) ister;
/// bu yüzden varsayılan `cargo test`'te koşmaz: `cargo test -p atlas-plugin -- --ignored`.
#[test]
#[ignore = "wasm32-unknown-unknown hedefi ve bağımlılık indirmesi gerekir"]
fn example_plugin_builds_from_source() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/example_momentum");
    let target = std::env::temp_dir().join("atlas-plugin-example-build");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let st = std::process::Command::new(cargo)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--manifest-path",
        ])
        .arg(root.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target)
        .status()
        .unwrap();
    assert!(st.success(), "örnek plugin derlenemedi");
    let wasm = target.join("wasm32-unknown-unknown/release/example_momentum.wasm");
    let module = PluginModule::from_file(&wasm).unwrap();
    let mut s =
        PluginStrategy::new("example_momentum", module, INST, PluginLimits::default()).unwrap();
    let out = drive(&mut s, &breakout_closes(), &flat());
    assert_eq!(out[30].len(), 1);
    assert_eq!(s.stats().failures(), 0);
}

#[test]
fn example_plugin_hash_is_stable() {
    let m = PluginModule::from_bytes(EXAMPLE_WASM).unwrap();
    assert_eq!(m.hash().len(), 64);
    assert_eq!(
        m.hash(),
        PluginModule::from_bytes(EXAMPLE_WASM).unwrap().hash()
    );
}
