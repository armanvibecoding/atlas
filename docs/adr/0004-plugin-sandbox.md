# ADR 0004 — Plugin sandbox'ı (wasmtime, ABI v1)

- Durum: kabul (M4, 2026-10-31)

## Bağlam

Spec §4.3: DSL'in (genom) ifade edemediği fikirler için LLM'in yazdığı Rust `wasm32`'ye derlenir ve
yakıt + bellek limitiyle çalışır. Host yalnız `ctx` okuma ve `Intent` döndürme açar; ağ/dosya/saat yok,
determinizm korunur. M4 kabulü: sandbox ağ/dosya/saat erişimini reddeder (test). Plugin kodu ve çıktısı
LLM ürünüdür → güvenilmez (kural 9).

## Seçenekler

| Seçenek | Artı | Eksi |
|---|---|---|
| **wasmtime** | Spec'te listeli; yakıt ölçümü, `StoreLimits`, NaN canonicalization yerleşik; Bytecode Alliance bakımı, güvenlik süreci olgun | Bağımlılık ağacı büyük, derleme süresi uzar |
| wasmer | Benzer özellikler | Spec dışı; yakıt "metering" ara katmanla, determinizm ayarları daha dağınık |
| wasmi (yorumlayıcı) | Küçük, saf Rust, deterministik | Yavaş (backtest'te her barda çağrı); spec dışı |
| Süreç izolasyonu (ayrı proses + seccomp) | Tam dil özgürlüğü | Platforma bağlı (Windows geliştirme), determinizm ve kaynak sınırı zor, IPC maliyeti |

## Karar

### Motor: `wasmtime` 50.0.1, varsayılan özellikler kapalı
Yalnız `cranelift`, `runtime`, `std`, `wat` açık. Kapalı kalanlar bilinçli: `async`, `gc`, `component-model`,
`threads`, `cache`, `profiling`, `pooling-allocator`, `parallel-compilation`. WASI crate'i (`wasmtime-wasi`)
**hiç bağımlılık değil**: host'un sunabileceği bir WASI fonksiyonu kodda yoktur.

### Neden WASI yok
WASI'nin her yüzeyi (fd, path, sock, clock, random, environ, args) ya yan etkidir ya da deterministik
olmayan girdidir. Strateji yalnız `ctx`'ten okumalı (kural 4) ve aynı girdiyle aynı sonucu vermelidir
(kural 2). Bu yüzden "kısıtlı WASI" bile verilmez:
- `PluginModule::from_bytes` modülün import listesini denetler; **tek bir import bile** (fonksiyon, bellek,
  tablo, global) varsa `ForbiddenImport { module, name }` döner.
- Savunma derinliği: instantiate boş bir `Linker` ile yapılır; statik denetim atlansa bile import çözülemez
  ve instantiate hata verir (birim testi var).
- `wasm32-unknown-unknown`'da `std::fs`, `std::net`, `std::time::SystemTime` import üretmez; "desteklenmiyor"
  hatası ya da panik döner (panik → `unreachable` → tuzak → sayılır), `println!` hiçbir şey yapmaz.
  `wasm32-wasip1` hedefi veya `wasm-bindgen` import üretir → modül yüklenmez.

### ABI v1
Modül dışa aktarır: `memory`, `alloc(len: i32) -> i32`, `on_event(ptr: i32, len: i32) -> i64`.
1. Host girdi JSON'unu serileştirir, `alloc(len)` çağırır, dönen adrese yazar (sınır dışıysa ABI hatası).
2. `on_event(ptr, len)` çağrılır. Dönüş: yüksek 32 bit çıktı adresi, düşük 32 bit uzunluk. 0 uzunluk = intent yok.
3. Host çıktıyı bellekten kopyalar (sınır dışı veya `max_output_bytes` üstü → ABI hatası).

Girdi (`PluginInput`): `abi`, `instrument`, `now` (UTC ns), `bars[]` (son N bar; `open_ts`, `close_ts`,
`open/high/low/close` tick cinsinden tam sayı, `volume`), `position` (`side`, `qty`, `entry`, `stop` ya da
`null`), `pending_entry`, `week_close_ns` (ders §9.6). Fiyatlar tick cinsinden tam sayıdır (kural 1).

Çıktı: JSON intent listesi. `open` (`side`, `qty`, `stop_loss` mutlak fiyat/tick, opsiyonel `take_profit`
mutlak fiyat/tick, opsiyonel `time_exit_bars`), `close`, `cancel_entry`. Şema katıdır
(`deny_unknown_fields`). Doğrulama: `qty ∈ [1, max_qty]`; stop alışta son kapanışın altında, satışta
üstünde; hedef karşı tarafta; `time_exit_bars ∈ [1, max]`; en fazla bir `open`; `open` yalnız düzken;
en fazla `max_intents` öğe. **Bir öğe bile geçersizse çıktının tamamı atılır** ve `invalid_outputs` artar.
Giriş her zaman piyasa emridir (limit/stop girişi ABI v1'de yok).

`Intent`'i host kurar: strateji kimliği (`plugin:<ad>#<wasm sha256 ilk 10>`), enstrüman, `ts = ctx.now()`,
`time_exit = now + n × bar süresi`, `close` gerekçesi sabit `"plugin"` (plugin metni journal'a girmez).
Plugin intent'i `RiskGate`'ten geçmeden emre dönüşmez; bu crate `risk`'e bağımlı değildir (kural 5).

### Limitler (`PluginLimits`, varsayılanlar)
| Limit | Varsayılan | Davranış |
|---|---|---|
| `fuel_per_event` | 50 000 000 | `alloc` + `on_event` toplamı; biterse `Trap::OutOfFuel` → `out_of_fuel` sayacı. `start` fonksiyonu da yakıtla sınırlı. |
| `max_memory_bytes` | 32 MiB | `StoreLimits`; başlangıç belleği büyükse instantiate düşer, büyüme limiti aşarsa tuzak (`trap_on_grow_failure`). Tek bellek, 1 örnek, 4 tablo. |
| `max_table_elements` | 10 000 | dolaylı çağrı tablosu |
| wasm yığını | 512 KiB | derin özyineleme tuzak olur |
| `window_bars` | 256 | girdideki son bar sayısı |
| `max_output_bytes` | 64 KiB | |
| `max_intents` / `max_qty` / `max_time_exit_bars` | 8 / 1000 / 100 000 | çıktı doğrulaması |
| `max_failures` | 10 | yakıt + tuzak + ABI + geçersiz çıktı toplamı bu sayıya ulaşınca plugin kalıcı devre dışı |

Hata = o olayda intent yok + sayaç (`PluginStats`); sessiz düşme yok, `last_error` tutulur.

### Determinizm ayarları
- `consume_fuel(true)`: kesme komut sayısına bağlı, duvar saatine değil. Aynı girdi → aynı yakıt tüketimi
  (testte ölçülüyor).
- `epoch_interruption(false)`: zaman tabanlı kesme yok (makineden makineye farklı noktada keserdi).
- `cranelift_nan_canonicalization(true)`: NaN bit desenleri platformdan bağımsız (testte `sqrt(-1)` ve
  `0/0` sonucunun `0x7ff8000000000000` olduğu doğrulanıyor).
- `wasm_relaxed_simd(false)` + `relaxed_simd_deterministic(true)`; thread/paylaşılan bellek derlenmedi;
  `multi_memory` ve `memory64` kapalı.
- Import yok → saat, rastgelelik, ortam yok.
- Örnek çağrılar arasında yaşar (plugin durum tutabilir); herhangi bir hatadan sonra atılır ve sonraki
  olayda temiz örnek kurulur. Girdi dizisi aynıysa bu da deterministiktir.
- Plugin yalnız kendi enstrümanının **bar** olaylarında, `bar.close_ts ≤ ctx.now()` ve veri akışı açıkken
  çağrılır (kesinti fiyat olayı değildir, ders §9.2).

## Sonuçlar
- M4 kabulünün plugin kısmı `crates/plugin/tests/sandbox.rs` ile otomatik: WASI `fd_write`, `path_open`,
  `sock_open/send/recv/accept`, `clock_time_get`, `random_get`, uydurma `env` fonksiyonu, bellek ve global
  importu reddediliyor; sonsuz döngü (çağrıda ve `start`'ta) yakıtla duruyor; bellek limiti aşımı
  reddediliyor; geçerli çıktı parse ediliyor, bozuk çıktı atılıp sayılıyor; determinizm ölçülüyor.
- Testler WAT metniyle yazıldığından `cargo test` wasm32 hedefi gerektirmez. Örnek plugin
  (`plugins/example_momentum`, workspace dışı) derlenmiş hâliyle `tests/fixtures/` altında; kaynaktan derleyen
  test `#[ignore]` (hedef + ağ ister).
- Bilinen sınırlamalar: girdi her barda JSON olarak yeniden serileştirilir (256 bar ≈ 25 KB); büyük taramalarda
  maliyet ölçülmeli, gerekirse ikili ABI v2. Plugin çağrı istatistikleri henüz journal/sicile yazılmıyor
  (çağıran `PluginStrategy::stats()` ile okur). Modül her `PluginModule` için kendi `Engine`'iyle derlenir;
  derleme önbelleği yok (`cache` özelliği kapalı).
- Yeni bağımlılık: `wasmtime` (spec §2'de listeli). Sürüm 50.0.1'e sabit; yükseltme ayrı PR + tam test
  (yakıt maliyet modeli sürümler arasında değişebilir → deney kaydı plugin hash'iyle birlikte değerlendirilir).
