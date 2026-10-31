# Değişiklik günlüğü

## M4 — Derinlik (2026-10-31)

### Eklendi
- **sim::MboQueueFill**: limit emirde kuyruk pozisyonu (L1 kotasyon + işlem akışı; MBO gelince kaynak değişir),
  piyasa etkisi; motor `observe`/`order_closed` kancalarıyla bağlı.
- **sim::calibration**: paper/canlı ↔ sim dolum eşleştirme, kayma ve limit dolum olasılığı kalibrasyonu,
  sürümlü `FillParams` (`queue@vN`), atomik sürüm dosyaları.
- **ml**: triple-barrier etiketleme, L2 lojistik regresyon (IRLS), purged k-fold + embargo, `MetaFiltered`
  (gir/girme + boyut), deterministik JSON model.
- **plugin**: wasmtime 50 sandbox (WASI yok, yakıt, bellek limiti, NaN kanonikleştirme, duvar saati yedeği),
  ABI v1, `PluginStrategy`, örnek plugin (wasm32).
- **dashboard**: axum paneli; veri `atlas --json` alt sürecinden (tek kaynak), salt-okunur beyaz liste,
  CSP, Host denetimi, yalnız localhost; kapsam testi CLI'daki her komutun sınıflandırılmasını zorunlu kılar.
- **CLI**: `verify m4`.

### Doğrulama
- `verify m4`: 4/4 — kalibrasyon farkı 2.02 → 0.64 tick; sandbox ağ/dosya/saat/rastgelelik reddi; dashboard
  27 CLI komutunun tamamını kapsıyor; meta-model AUC sinyal 0.87 / gürültü 0.51.

## M3 — Otonomi (2026-10-31)

### Eklendi
- **ops**: olay hunisi (journal → uyandırma → outbox), disk outbox (Idempotency-Key, üstel geri çekilme,
  ölü kutusu), sessiz saat 23:30–09:00 TSİ, dedupe, saatlik uyandırma limiti, gece özeti, saat kayması.
- **exec**: Binance 1m REST akışı (boşluk telafisi, kesinti ≠ fiyat), `ReplayFeed`, gölge/paper `Runner`.
- **portfolio**: SPRT + CUSUM sapma dedektörü, Thompson allocator, rejim.
- **registry**: terfi merdiveni (append-only), duraklat/devam/düşür izinleri, tek kullanımlık onaylar,
  ajan → arama kuyruğu, kaynak ilerleme istatistiği.
- **search**: evrim + Bayes (TPE benzeri) + kaynak meta-öğrenmesi; her tarama sicilde deneme.
- **agents**: Anthropic / xAI / sahte sağlayıcı, 4 rol (sürümlü prompt), şema doğrulama, bütçe kapısı.
- **daemon**: canlı + hızlandırılmış (sanal saat) mod, systemd watchdog, kalıcılık, checkpoint.
- **CLI**: `daemon run`, `report daily|weekly`, `strategy pause|demote|resume`, `budget`, `approvals list|show`,
  `approve`, `verify m3`. **deploy**: `atlas-admin`, `atlas.service`, `install.sh`, `logrotate.conf`.

### Doğrulama
- `verify m3`: 5/5 — Grok izinleri (9 izinli / 12 yasak + CLI ikinci kontrol), ajan şeması, sapma düşürmesi,
  72 sa sanal saat dayanıklılığı (0 outbox_dead, 0 engine_failure, journal tutarlı, webhook teslimi), kurulum.

### Güvenlik (otomatik inceleme sonrası)
- Duraklatılmış strateji düşürmeyle canlıya dönemez; onay tek kullanımlık; doğrulama koşusu gerçek daemon'un
  durdurma dosyasına ve LLM'e dokunmaz; strateji adı yalnız `strategies/` içinde (yol geçişi yok); plugin
  stop/hedef kontrolü taşmasız, CPU tüketimine duvar saati yedeği.

## M2 — Doğruluk makinesi (2026-10-31)

### Eklendi
- **features**: 32 feature türü (ATR, EMA/SMA, RSI, VWAP, CVD, delta, imbalance, absorption, PDH/PDL,
  Asya/Londra aralığı, açılış aralığı, hacim profili POC/VAH/VAL, sweep, CVD uyumsuzluğu, highest/lowest…),
  `available_at` damgalı sütunlar, (veri versiyonu × feature) önbelleği, lookahead dedektörü.
- **genome**: TOML strateji DSL'i, derleyici (`GenomeStrategy`, paylaşılan sütun ya da canlı mod),
  rastgele genom, mutasyon, çaprazlama, budama, parametre komşuluğu, yapısal mesafe.
- **registry**: append-only deney sicili, geri alınamaz deneme sayacı, ders (otopsi) tablosu, benzer ölü fikir
  sorgusu, holdout erişim bütçesi.
- **gauntlet**: 10 kapının tamamı (örneklem, purged+embargoed CPCV, walk-forward, Deflated Sharpe, PBO/CSCV,
  parametre stabilitesi, maliyet stresi, rejim kırılımı, prop Monte Carlo, holdout), `GauntletReport` (JSON +
  Markdown), sahte strateji testi (paralel, deterministik).
- **5 referans strateji** (`strategies/`): trend_pullback, sweep_mss, vwap_mean_reversion,
  opening_range_breakout, cvd_divergence.
- **CLI**: `gauntlet run|eval [--frictionless]`, `strategies list|show`, `experiments summary`, `verify m2`,
  `data measure-slippage`.
- **Kalibrasyon**: Binance kayması ölçüldü (aggTrades, yarım spread) → BTCUSDT/ETHUSDT 1 tick.
- **Veri**: depolama formatı v2 (taker alış hacmi); BTCUSDT + ETHUSDT yeniden çekildi.

### Doğrulama
- `verify m2`: 10/10 YEŞİL. Sahte strateji testi 1000 aday, gerçek maliyetle 0 geçen; sürtünmesiz 0 geçen
  (kapı 1: 407, 2: 193, 3: 211, 4/DSR: 189).
- Güç testi (CI): bilinçli yerleştirilmiş momentum edge'i 10 kapıyı geçiyor, aynı genom gürültüde 2. kapıda ölüyor.
- Referanslar (BTCUSDT 1m, in-sample 921 bin bar): beşi de kapı 2'de düştü. Maliyetsiz beklentiler
  trend_pullback +0.057R, cvd_divergence +0.054R, vwap_mean_reversion +0.033R; gerçek taker ücreti işlem başına
  ~0.85–1.3R → 1m BTC'de dar stop + piyasa emri yapısal olarak kaybettiriyor.
- `verify m1` 24/24 yeşil kaldı. 104 test, clippy temiz.

## M1 — Temel (2026-10-31, devam)

### Eklendi
- **Workspace**: Rust 2024, `core`, `risk`, `journal`, `data`, `sim` crate'leri ve `atlas` CLI; clippy
  (`unwrap/expect/panic` yasak), rustfmt, GitHub Actions CI.
- **core**: `Price(i64)` tick, `Qty(i32)`, `Money(i64)` mikro-dolar, `UnixNanos`; ondalık→tamsayı ayrıştırma
  (f64 yok); `Instrument` + `instruments.toml` yükleyici (maliyetler uydurulmaz, boş = hata);
  `calendar` (haftalık seans, günlük ara, DST, tatil aralıkları, sonraki kapanış/hafta kapanışı);
  piyasa olayları (bar olayı kapanış anında); sistem olayları (§5 öncelikleri); `Intent`, `Fill`;
  `Strategy` trait'i + `Ctx` (yalnız işlenmiş geçmiş; seans/hafta sonuna kalan süre).
- **risk**: prop config yükleyici (boş alan → yüklenmez, hepsi listelenir); `static/eod_trailing/
  intraday_trailing` taban + kilit; günlük zarar (gün başı bakiye/equity); consistency kontrolü + ileriye
  dönük tavan; kontrat tavanı (mini/mikro ağırlıklı); grup net maruziyet; stop sonrası cooldown; haber
  yasağı; firma saatleri (`no_new_entries_after`, `flat_by`); hafta sonu; kill switch (global/hesap/strateji);
  yalnız-sıkılaştırma politika API'si; LLM bütçe izleyici. `proptest`: hiçbir Intent dizisi tavanları aşamaz.
- **journal**: append-only SQLite (UPDATE/DELETE tetikleyiciyle reddedilir), projeksiyon, checkpoint hash'i,
  `replay` doğrulaması, backup API ile anlık görüntü.
- **data**: `.atb` depolama + `DataVersion` (ADR 0001), manifest hash kontrolü, kalite monitörü (boşluk,
  sıra, yinelenen, OHLC, sıfır hacim, kilitli/çapraz kitap), Binance USDⓈ-M 1m çekici (exchangeInfo ile
  tick/lot doğrulaması), Databento maliyet sorgusu + tavan kapısı, `DataSource` + `StoreSource`.
- **sim**: olay tabanlı motor, `BarFill`/`TradeFill`, OCO, gecikme, maliyet çarpanları, zorunlu çıkışlar
  (kill, flat_by, hafta sonu, zaman), kesinti ≠ fiyat; sentetik veri; M1 doğrulamaları; prop uyum testleri.
- **CLI**: `status`, `health`, `events`, `verify m1`, `journal replay|export|checkpoint`,
  `data list|pull-binance|quote-databento`, `kill`, `kill-reset`, `note`; `--json` (`atlas.cli/1`).

### Doğrulama
- `verify m1`: 24/24 YEŞİL (genel prop kuralları, configs/props/generic_100k.toml).
- Gerçek veri: BTCUSDT 1.050.655 + ETHUSDT 1.050.665 bar; rastgele giriş maliyetsiz ≈ 0 (p=0,42), al-tut birebir.

### Güvenlik (otomatik inceleme sonrası)
- RiskGate: tetiklenmiş stop girişlerinde risk, en kötü dolum fiyatından (`risk_entry_ref`) hesaplanır.
- CLI: `journal export` yalnız `var/exports/<ad>`, üzerine yazmaz; Grok için `--home` yasak;
  kill gerekçesi/not/strateji kimliğinde kontrol karakteri reddedilir, journal metni ekrana arındırılarak basılır.
