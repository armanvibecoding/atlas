# Atlas — Claude Code yapım spesifikasyonu

> Okuyucu: Claude Code (yazılımcı). Sahibi: Arman. Operatör: Grok Bot (ayrı belge: `GROK_OPS.md`).
> Bu repo sıfırdan yazılır. Eski `paper-challenge` (Python) kodu **taşınmaz**; yalnızca §9'daki dersler alınır.
> Rol ayrımı: **Claude Code inşa eder, Grok Bot işletir, Arman onaylar.** Grok Bot kod yazmaz; onun sisteme tek erişimi §6'daki `atlas` CLI'dir. O arayüz bu projenin bir ürün parçasıdır, ihmal edilmez.
> **Yapım tek seferde yapılır:** Claude Code M1→M4'ü ara teslim olmadan, sırayla bitirir. Grok Bot yapım sırasında sisteme dokunmaz; sistemi yalnızca §7.1'deki devir koşulları sağlandığında, tamamlanmış hâliyle tek seferde devralır.

---

## 0) Amaç

Kendini geliştiren, otonom bir futures/kripto araştırma ve işlem platformu:
- Strateji hipotezi üretir, test eder, **sahte olanı öldürür**, gerçek olanı kademeli olarak canlıya terfi ettirir.
- Hedef metrik: **prop hesabı geçme olasılığı** ve canlıda pozitif beklenti. Sharpe tek başına hedef değildir.
- Sistemin asıl zekâsı strateji üretmek değil, **neyin gerçek olduğuna karar veren gauntlet**tir (§4.4). Arama ne kadar büyürse sahte keşif o kadar artar; gauntlet bunu kontrol eder.

Kapsam dışı: gerçek parayla canlı işlem (M5'e kadar), yüksek frekanslı işlem (HFT), opsiyonlar.

---

## 1) Değişmez kurallar (ihlal = PR reddi)

1. **Fiyat asla f64 değil.** `Price(i64)` enstrümanın tick'i cinsinden. `Qty(i32)` kontrat. Para `Money(i64)` mikro-dolar. f64 yalnızca istatistik/feature hesabında.
2. **Determinizm.** Aynı veri versiyonu + aynı config + aynı seed → bit düzeyinde aynı sonuç. Tüm rastgelelik `rand_chacha` + açık seed. `HashMap` iterasyon sırası sonuca etki edemez (`BTreeMap` veya `IndexMap`).
3. **Backtest ve canlı aynı kod yolundan geçer.** Değişen yalnız `DataSource` ve `Executor` implementasyonu. Strateji kodunda `if backtest` yasak.
4. **Lookahead imkânsız olmalı.** Strateji yalnızca `ctx`'ten okur; `ctx` olay zamanına kadarki veriyi verir. Feature'lar `available_at` damgası taşır. Testte lookahead dedektörü (§4.2) zorunlu.
5. **`risk` crate'i anayasadır.** Her emir `RiskGate`'ten geçer. Strateji, ajan veya plugin bu crate'e bağımlılık dışında erişemez; limitleri çalışma anında gevşetecek API yoktur (yalnız sıkılaştırma).
6. **Journal append-only.** Hiçbir olay silinmez/güncellenmez. Durum olaylardan yeniden üretilebilir (`atlas journal replay` ile doğrulanır).
7. **Her deney sicile yazılır** — başarısızlar dahil. Çoklu test düzeltmesi toplam deneme sayısıyla yapılır.
8. **Holdout kilitli.** Holdout verisine erişim yalnız `gauntlet::holdout` üzerinden, her erişim sayılır ve sicile yazılır.
9. **LLM çıktısı güvenilmez girdidir.** Ajan çıktısı şemaya göre parse edilir; geçersizse atılır. Ajanlar `risk`, `journal` yazımı (not hariç), canlı config ve sermayeye dokunamaz.
10. **Secret'lar repo'da yok.** `/etc/atlas/atlas.env` (600), kodda `ATLAS_*` env okunur.

---

## 2) Teknoloji

- Rust stable, edition 2024. Cargo workspace. `cargo clippy -- -D warnings`, `rustfmt` zorunlu.
- Bağımlılıklar (gerekçesiz yenisi eklenmez; eklenirse `docs/adr/` altına ADR):
  - Çalışma zamanı: `tokio`, `clap`, `serde`, `toml`, `serde_json`, `tracing`, `tracing-subscriber`, `thiserror`, `anyhow` (yalnız binary'lerde)
  - Zaman: `chrono`, `chrono-tz` (CME = `America/Chicago`)
  - Veri: `databento`, `dbn`
  - Depolama: `rusqlite` (bundled) — journal ve sicil. Bar/feature depolama formatı M1'de ADR ile seçilir (DBN dosyaları + kendi kolon formatı ya da Parquet).
  - Hesap: `rayon`, `rand_chacha`, `statrs`
  - Sandbox: `wasmtime` (plugin'ler, §4.3)
  - Web: `axum`, `reqwest`
  - Test: `proptest`, `insta`, `criterion`
- Kütüphane crate'lerinde `unwrap()`/`expect()` yok (testler hariç). Panik = bug.
- Zaman tipi: `UnixNanos(u64)` UTC. Takvim/seans dönüşümü yalnız `core::calendar`'da.

---

## 3) Workspace

```
atlas/
├── crates/
│   ├── core/        tipler, Instrument, calendar, olay tipleri
│   ├── data/        DataSource trait; Databento (MBO, trades, 1m), kripto L2 WS, ekonomik takvim; veri versiyonlama
│   ├── features/    point-in-time feature store
│   ├── sim/         event-driven motor, simüle borsa, kuyruk/gecikme/etki modelleri, fill kalibrasyonu
│   ├── risk/        RiskGate, prop kuralları, kill switch, bütçeler
│   ├── genome/      strateji DSL'i + evrimsel operatörler
│   ├── plugin/      WASM strateji eklentileri (wasmtime)
│   ├── ml/          meta-labeling
│   ├── gauntlet/    doğrulama: CPCV, walk-forward, DSR, PBO, stres, Monte Carlo prop-geçme, holdout
│   ├── registry/    deney sicili + bilgi tabanı
│   ├── agents/      LLM ajanları (araştırmacı, eleştirmen, mühendis, raporcu); LlmProvider trait
│   ├── portfolio/   allocator, drift dedektörü, rejim
│   ├── exec/        Executor trait: Sim, Paper, ileride Rithmic/Tradovate
│   ├── journal/     append-only olay kaynağı (SQLite)
│   ├── ops/         daemon, sağlık, outbox/webhook, sessiz saatler
│   └── dashboard/   Axum web arayüzü + JSON API
├── bins/atlas/      tek CLI (§6)
├── configs/         props/*.toml, instruments.toml, atlas.toml
├── strategies/      genom TOML'leri
├── plugins/         WASM eklenti kaynakları (her biri ayrı crate, wasm32 hedefi)
├── docs/adr/        mimari karar kayıtları
└── deploy/          systemd, logrotate, atlas-admin (SSH zorunlu komut sarmalayıcı)
```

Bağımlılık yönü: `core` ← herkes. `risk` yalnız `core`'a bağlı. `agents` hiçbir zaman `risk`/`exec`'e bağlı değil; yalnız `registry` ve `genome` şemalarını bilir.

---

## 4) Katmanlar

### 4.1 Veri (`data`)
- `trait DataSource { fn events(&mut self, range) -> impl Iterator<Item = MarketEvent> }` — backtest ve canlı aynı tipte olay üretir.
- Kaynaklar: Databento CME (NQ, MNQ, ES, MES, GC, MGC, CL; MBO + trades + 1m OHLCV), kripto L2 (Binance/Bybit WS, kayıt + replay), ekonomik takvim.
- **Maliyet kontrolü:** Databento indirmesinden önce maliyet sorgusu yapılır; `atlas.toml`'daki `data.max_usd_per_pull` aşılırsa indirme durur ve onay ister.
- Her veri seti içerik hash'iyle versiyonlanır (`DataVersion`). Deney kayıtları bu hash'i taşır.
- Veri kalite monitörü: boşluk, sıra dışı zaman damgası, kilitli/çapraz kitap, sıfır hacim. Hata = olay (`data_quality`), sessiz düzeltme yok.
- Ders (§9): kaynak kesintisi asla fiyat olayı gibi yorumlanmaz.

### 4.2 Feature store (`features`)
- Her feature: ad, versiyon, girdi bağımlılıkları, `available_at` damgası. Hesap bir kez yapılır, önbelleğe yazılır.
- **Lookahead dedektörü:** test modunda feature değerini zaman kaydırılmış veriyle yeniden hesaplar; gelecekteki veriye duyarlılık varsa test kırılır.
- Başlangıç feature seti: ATR, EMA ailesi, VWAP, seans seviyeleri (PDH/PDL, Asya/Londra aralığı), hacim profili (POC/VAH/VAL), CVD, delta, absorption göstergesi, imbalance, kitap derinliği oranı.

### 4.3 Strateji temsili
- **`trait Strategy { fn on_event(&mut self, ctx: &Ctx, ev: &MarketEvent) -> Vec<Intent> }`** — strateji emir değil `Intent` üretir; `RiskGate` ve `Executor` emre çevirir.
- **Genom (`genome`)**: TOML ile ifade edilen birleşik yapı: `entry` (sinyal ağacı), `filters`, `exit` (stop/hedef/zaman/izleyen), `sizing`, `session`. Primitive'ler kapalı bir kümeden gelir ve her biri tiplenmiş parametre aralığına sahiptir. Evrimsel operatörler: mutasyon (parametre, primitive takası), çaprazlama, budama. Genom → derlenmiş `Strategy`.
- **Plugin (`plugin`)**: DSL'in ifade edemediği fikirler için LLM'in yazdığı Rust → `wasm32` derleme → `wasmtime` içinde yakıt (fuel) ve bellek limitiyle çalışır. Host yalnız `ctx` okuma ve `Intent` döndürme fonksiyonu açar; ağ/dosya/saat yok. Determinizm korunur.
- **ML (`ml`)**: meta-labeling. Birincil sinyal (genom/plugin) aday üretir; ikincil model "gir/girme + boyut" verir. Etiketleme triple-barrier. Kütüphane seçimi M4'te ADR ile.

### 4.4 Simülasyon (`sim`)
- Olay kuyruğu tabanlı motor; tüm bileşenler aynı saatten beslenir.
- Fill modelleri (trait `FillModel`): `BarFill` (hızlı tarama), `TradeFill`, `MboQueueFill` (limit emirde kuyruk pozisyonu; önündeki hacim tükenmeden dolum yok). Gecikme modeli ve basit piyasa etkisi.
- Maliyet: kontrat başına komisyon + borsa ücreti (`instruments.toml`), kayma modeli.
- **Kalibrasyon:** paper/canlı dolumlar simülasyondaki karşılıklarıyla eşleştirilir; kayma ve dolum olasılığı parametreleri güncellenir, sürüm olarak sicile yazılır. Kalibre parametre değişikliği geçmiş sonuçları geçersiz kılmaz; deney hangi fill-model versiyonunu kullandığını kaydeder.
- **Doğrulama testleri (M1 kabul):** rastgele giriş stratejisinin beklenen sonucu ≈ −maliyet (istatistiksel test); al-tut stratejisi fiyat farkını birebir verir; aynı mumda stop+hedef → stop; gap'te stop dolumu açılış fiyatından.

### 4.5 Risk (`risk`)
- `RiskGate::check(&State, &Intent) -> Decision` saf fonksiyon; yan etkisiz, %100 test kapsaması, `proptest` ile özellik testleri.
- Prop kuralları `configs/props/<firma>_<boyut>.toml`'dan: başlangıç bakiyesi, drawdown tipi (`static` / `eod_trailing` / `intraday_trailing`), drawdown miktarı, kilitlenme seviyesi, günlük zarar limiti, consistency kuralı (tek günün kârı / toplam kâr ≤ oran), maksimum kontrat, işlem saatleri, haber yasağı penceresi.
- **Prop config değerleri uydurulmaz.** Şablonda `# ARMAN DOLDURUR — firma kuralından teyit` işaretli boş alanlar olur; boş alan varsa o config yüklenmez.
- Consistency kuralı yalnız kontrol değil, **ileriye dönük** de uygulanır: bugünkü kâr tavanı hesaplanır, yaklaşınca yeni giriş kısılır (`consistency_headroom` olayı).
- Kill switch'ler: hesap düzeyi, strateji düzeyi, global. Tetiklenince açık pozisyonlar kapanır ve manuel açma gerekir.
- LLM günlük token/USD bütçesi de burada tanımlıdır.

### 4.6 Gauntlet (`gauntlet`) — sistemin çekirdeği
Bir strateji adayı şu kapılardan **sırayla** geçer; ilk düştüğü kapıda durur ve otopsi kaydı yazılır:

| # | Kapı | Kural (varsayılan, `atlas.toml`'da sıkılaştırılabilir) |
|---|---|---|
| 1 | Asgari örneklem | ≥ 100 işlem (in-sample), ≥ 30 işlem her CPCV test dilimi toplamında |
| 2 | Purged + embargoed CPCV | Test dilimlerinin medyan beklentisi > 0, dilimlerin ≥ %60'ı pozitif |
| 3 | Walk-forward | Ardışık OOS pencerelerinde beklenti > 0 |
| 4 | Deflated Sharpe Ratio | Sicildeki **toplam deneme sayısı** ile DSR olasılığı ≥ 0,95 |
| 5 | PBO (CSCV) | Aşırı uyum olasılığı ≤ 0,20 |
| 6 | Parametre stabilitesi | Her parametrenin ±%20 komşuluğunda beklenti pozitif kalır (plato, tepe değil) |
| 7 | Maliyet stresi | 2× kayma + 2× komisyonla beklenti ≥ 0 |
| 8 | Rejim kırılımı | Tek rejim toplam PnL'in > %70'ini üretiyorsa düşer |
| 9 | Prop Monte Carlo | Hedef prop config'inde geçme olasılığı ≥ eşik, patlama olasılığı ≤ eşik (eşikler Arman'ın) |
| 10 | Holdout | Kilitli son dönem; her erişim holdout bütçesinden düşer, bütçe bitince yeni holdout dönemi gerekene kadar terfi yok |

- **Sahte strateji testi (M2 kabul, zorunlu):** saf gürültü üreten veya geleceğe bakmayan ama rastgele parametre aranmış 1.000 strateji gauntlet'ten geçirilir; geçen oran ≤ %1 olmalı. Bu test CI'da her sürümde koşar.
- Gauntlet sonuçları yapılandırılmış (`GauntletReport`) ve insan-okunur (Markdown) olarak sicile yazılır.

### 4.7 Sicil ve bilgi tabanı (`registry`)
- `experiments` tablosu: id, ebeveyn id, kaynak (insan/evrim/ajan-X/prompt-versiyonu), genom/plugin hash, veri versiyonu, fill-model versiyonu, seed, gauntlet sonucu, düştüğü kapı, maliyet (CPU sn, LLM token).
- `lessons` tablosu: otopsiden çıkan yapılandırılmış ders (ne denendi, neden öldü, hangi rejimde çalıştı). Ajanlar yeni hipotezden önce buna benzerlik sorgusu yapar; çok benzer ölü fikir tekrar denenmez (benzerlik: genom yapısal mesafesi + etiketler).
- Deneme sayacı global ve geri alınamaz (kural 7).

### 4.8 Ajanlar (`agents`)
- `trait LlmProvider` — sağlayıcıdan bağımsız (Anthropic, xAI vb.), config'ten seçilir. Her çağrı token/USD olarak `risk` bütçesinden düşer; bütçe biterse ajanlar durur, sistem mekanik çalışmaya devam eder.
- Roller (her biri ayrı, versiyonlu prompt dosyası `agents/prompts/*.md`):
  - **Araştırmacı:** journal özeti + rejim raporu + bilgi tabanı → yapılandırılmış hipotez (JSON şeması: gerekçe, genom taslağı, beklenen davranış, çürütme kriteri).
  - **Eleştirmen:** her hipoteze "neden sahte olabilir" + kontrol deneyi önerir. Eleştirmen onayı olmadan hipotez arama kuyruğuna girmez.
  - **Mühendis:** plugin kaynağı ve platform iyileştirme PR'ı yazar. PR'lar yalnız ayrı branch'e gider; merge insan işidir.
  - **Raporcu:** günlük/haftalık Türkçe özet (Grok Bot da bunu tüketir).
- Ajan çıktısı JSON şemasıyla doğrulanır; geçersiz çıktı sayılır ve atılır.

### 4.9 Arama
- Evrimsel döngü: popülasyon genomları → hızlı tarama (`BarFill`, kısa dönem) → elit seçim → tam gauntlet. Hızlı taramada da her değerlendirme sicile yazılır (deneme sayısına girer).
- Parametre ayarı: Bayesian optimizasyon, deney başına bütçe (CPU sn).
- Arama bütçesi kaynaklara (insan, evrim, araştırmacı ajan, prompt versiyonları) **meta-öğrenme** ile dağıtılır (§4.11).

### 4.10 Portföy (`portfolio`)
- Allocator: canlı stratejiler arası risk dağıtımı; zamanla azalan ağırlıklı Thompson sampling. Strateji başı ve toplam risk tavanı `risk`'ten gelir.
- Drift dedektörü: canlı işlem sonuçları gauntlet'in tahmin ettiği dağılıma karşı CUSUM + SPRT. Alarm → otomatik düşürme (bir basamak aşağı).
- Rejim tespiti: volatilite ve trend durumu; stratejiler gauntlet'te çalıştıkları rejimlerle etiketlidir, rejim dışında kısılır.
- Korelasyon/net maruziyet kontrolü (ders §9).

### 4.11 Kendini geliştirme
- **L1 strateji:** §4.3–4.9.
- **L2 araştırma süreci:** her kaynağın (ajan, prompt versiyonu, evrim operatörü) ürettiği adayların *terfi merdiveninde ne kadar yükseldiği* izlenir; arama bütçesi buna göre yeniden dağıtılır. Prompt'lar versiyonlanır, yeni prompt versiyonu A/B olarak bütçe alır.
- **L3 altyapı:** fill kalibrasyonu (§4.4), veri kalite kuralları, mühendis ajanın platform PR'ları. Her PR: test + benchmark + "sahte strateji testi" CI'dan geçer, merge Arman onayıyla.

### 4.12 Terfi merdiveni
`aday → gauntlet → gölge (canlı veri, emir yok) → paper → mikro eval → ölçek`
- Her geçiş istatistiksel testle otomatik (SPRT: yeterli kanıt varsa terfi, ters kanıt varsa düşürme).
- **İnsan onayı zorunlu 3 durum:** (1) risk limiti değişikliği, (2) canlıya yeni kod (merge), (3) mikro eval ve sonrası sermaye artışı. Bunlar için sistem `approval_request` olayı ve onay paketi üretir; onay yalnız `atlas approve <id>` ile, yalnız Arman'ın anahtarıyla.

---

## 5) Olaylar

`core::events` tek kaynak. Her olay: `id`, `ts`, `kind`, `priority`, `key` (dedupe), `payload`.
- **HIGH:** `kill_switch`, `risk_breach`, `daily_loss_limit`, `prop_drawdown_near`, `consistency_headroom`, `feed_down`, `engine_failure`, `clock_skew`, `drift_alarm`, `approval_request`, `budget_exhausted`, `outbox_dead`
- **MEDIUM:** `position_opened`, `position_closed`, `promotion`, `demotion`, `gauntlet_pass`, `data_quality`, `fill_divergence`
- **INFO:** diğerleri (`heartbeat`, `experiment_done`, …). INFO tek başına uyandırmaz; **asla sessizce düşürülmez** (ders §9).
- Webhook teslimi `ops::Outbox`: disk tabanlı, `Idempotency-Key`, üstel geri çekilme, ölü kutusu. Uyandırma limiti, dedupe, sessiz saatler (23:30–09:00 TSİ) ve gece özeti `atlas.toml`'dan.

---

## 6) Grok Bot arayüzü: `atlas` CLI (ürün parçası)

Grok Bot sisteme yalnız SSH ile ve yalnız `deploy/atlas-admin` zorunlu komut sarmalayıcısıyla bağlanır (authorized_keys `command=`). Sarmalayıcı aşağıdaki beyaz listeyi uygular. Her komut `--json` destekler, çıktı şeması `docs/cli-schema.md`'de sabittir ve sürümlüdür.

| Komut | İş | Grok izni |
|---|---|---|
| `status`, `health` | sağlık; çıkış kodu 0 sağlıklı | ✓ |
| `report daily|weekly` | raporcu çıktısı + metrikler | ✓ |
| `events [--since] [--prio]` | olay kuyruğu | ✓ |
| `strategies list|show <id>` | merdivendeki stratejiler | ✓ |
| `strategy pause <id> <gerekçe>` | stratejiyi duraklat (yalnız kısıtlayıcı) | ✓ |
| `strategy demote <id> <gerekçe>` | bir basamak düşür | ✓ |
| `strategy resume <id> <gerekçe>` | yalnız Grok'un kendi duraklattığını geri açar | ✓ |
| `kill <scope> <gerekçe>` | kill switch tetikle | ✓ |
| `note <metin>` | journal'a operatör notu (stdin) | ✓ |
| `experiments summary` | sicil özeti, deneme sayısı, kaynak performansı | ✓ |
| `budget` | LLM/veri/CPU bütçesi durumu | ✓ |
| `approvals list|show <id>` | bekleyen onay paketleri | ✓ |
| `verify m1..m5` | kilometre taşı kabul testleri | ✓ |
| `approve <id>` | onay | ✗ yalnız Arman |
| `risk set …`, `deploy`, `kill-reset` | — | ✗ yalnız Arman |

Grok'un hiçbir komutu riski artıramaz, terfi ettiremez veya sermaye değiştiremez. Bu, sarmalayıcıda ve CLI'da çift kontrolle uygulanır; testleri vardır.

---

## 7) Kilometre taşları

Taşlar Grok'a ara teslim değil, **Claude Code'un iç kalite kapılarıdır.** Her taşın kabul kriteri `atlas verify mN` komutuyla otomatik kontrol edilir; Claude Code kriteri kendisi koşturur ve geçmeden sonraki taşa geçmez. Bir taşta takılırsa Arman'a sorar, kriteri gevşetmez.

### M0 — Ön koşullar (Arman, kod yok; M1 başlamadan önce)
- Hedef prop firmasının **otomatik/algoritmik işlem politikasını** yazılı teyit et. İzin yoksa M5 hedefi değişir. (Engel: M5)
- Hedef prop config'inin değerlerini firma kurallarından doldur.
- Databento hesabı + bütçe tavanı.

### M1 — Temel (≈3 hafta): `core`, `risk`, `data`, `sim`, `journal`, `bins/atlas` iskeleti
Görevler sırayla:
1. Workspace, CI (fmt, clippy, test), `docs/adr/0001-storage-format.md`.
2. `core`: tipler, `Instrument` (tick boyutu, tick değeri, seans), `calendar` (CME seansları, DST, tatiller), olay tipleri.
3. `risk`: `RiskGate`, prop config yükleyici (boş alan → hata), tüm drawdown tipleri, consistency (kontrol + ileriye dönük tavan), kill switch. `proptest` ile: "hiçbir Intent dizisi limiti aşan pozisyon üretemez".
4. `journal`: append-only SQLite, replay ile durum yeniden üretimi.
5. `data`: Databento indirme (maliyet sorgusu + tavan), yerel depolama, versiyon hash'i, kalite monitörü. NQ/MNQ ve GC/MGC, 2 yıl, 1m + trades.
6. `sim`: motor, `BarFill`, `TradeFill`, maliyet modeli.
7. `atlas` CLI: `status`, `health`, `events`, `verify m1`.

**Kabul:** rastgele strateji testi ve al-tut testi geçer; prop kural testleri Arman'ın doldurduğu config'le birebir uyar; aynı seed ile iki koşu bit düzeyinde aynı; `journal replay` tutarlı.

### M2 — Doğruluk makinesi (≈3 hafta): `features`, `genome`, `gauntlet`, `registry`
1. Feature store + lookahead dedektörü.
2. Genom DSL'i + derleyici; 5 elle yazılmış referans strateji (trend-pullback, sweep+MSS, VWAP mean-reversion, açılış aralığı kırılımı, CVD diverjansı).
3. Gauntlet 10 kapının tamamı + `GauntletReport`.
4. Sicil + deneme sayacı + bilgi tabanı tabloları.
5. CLI: `strategies`, `experiments summary`, `verify m2`.

**Kabul:** sahte strateji testinde geçme oranı ≤ %1; 5 referans stratejinin gauntlet raporları üretilmiş (geçip geçmemeleri önemli değil).

### M3 — Otonomi (≈3 hafta): `agents`, arama, `exec::Paper`, `ops`, `portfolio` (drift)
1. `LlmProvider`, 4 ajan rolü, JSON şemaları, bütçe.
2. Evrimsel arama + Bayesian ayar + kaynak takibi.
3. Gölge ve paper modu (canlı veri; Databento live ve kripto WS), outbox/webhook, sessiz saatler, gap telafisi.
4. Drift dedektörü, otomatik düşürme.
5. `atlas-admin` sarmalayıcı + Grok komutlarının tamamı + izin testleri.
6. systemd birimi (watchdog, kaynak limitleri, sertleştirme), `deploy/install.sh`.

**Kabul:** sistem 72 saat insan müdahalesi olmadan çalışır; deney üretir, eler; sıfır `outbox_dead`; Grok izin testleri yeşil. (7 günlük tam deneme §7.1'de, tamamlanmış sistemle yapılır.)

### M4 — Derinlik (≈4 hafta): `sim::MboQueueFill`, fill kalibrasyonu, `ml`, `plugin`, `portfolio` tam, meta-öğrenme, `dashboard`
**Kabul:** paper dolumlarıyla simülasyon arasındaki kayma farkı ölçülür ve kalibrasyon sonrası küçülür; plugin sandbox'ı ağ/dosya/saat erişimini reddeder (test); dashboard tüm CLI verisini gösterir.

### 7.1 Devir: Grok'a tek seferde teslim
Devir yalnız şu koşulların **hepsi** sağlandığında yapılır:
1. `atlas verify m1`, `m2`, `m3`, `m4` VPS üzerinde yeşil.
2. **7 günlük kesintisiz deneme:** tamamlanmış sistem VPS'te gölge + paper modunda 7 gün insan müdahalesi olmadan çalışır. Kriterler: sıfır `engine_failure` tekrarı (saatte ≥3), sıfır `outbox_dead`, sıfır sessiz düşen olay, `journal replay` tutarlı, en az bir aday gölgeye çıkmış (çıkmadıysa raporun nedeni açıklaması yeterli).
3. Grok'un tüm beyaz liste komutları VPS'te `atlas-admin` üzerinden çalışıyor; izin testleri (yasak komutların reddi) yeşil.
4. Webhook Grok'un alarm routine'ine bağlı ve test olayı teslim edilmiş.
5. `GROK_OPS.md` son CLI ve olay setiyle güncel.
6. `HANDOFF.md` yazılmış: sistemin durumu, açık sorunlar (`docs/OPEN_QUESTIONS.md`), canlıdaki stratejiler, bütçeler, bilinen sınırlamalar, ilk hafta Grok'un dikkat etmesi gerekenler.

Koşullar sağlanınca Arman devri onaylar (`atlas approve handoff`). Devirden sonra Claude Code yalnız Arman'ın açtığı görevlerle çalışır (M5, hata düzeltme, iyileştirme); canlı sisteme doğrudan müdahale etmez, değişiklikler deploy yoluyla gelir.

### M5 — Eval (devirden sonra)
Terfi merdivenini otomatik geçen ilk strateji için `approval_request`; Arman onaylarsa mikro eval hesabı. Executor: hedef firmanın platformu (Rithmic R|Protocol veya Tradovate API) — M0 sonucuna göre ADR.

---

## 8) Çalışma biçimi

- Her taş kendi branch'inde; küçük, testli PR'lar. Her PR açıklaması: ne, neden, nasıl test edildi.
- Mimari karar → `docs/adr/NNNN-başlık.md` (bağlam, seçenekler, karar, sonuçlar).
- Belirsizlikte uydurma; `docs/OPEN_QUESTIONS.md`'ye yaz ve Arman'a sor. Özellikle prop kuralları, maliyetler ve veri bütçesi uydurulmaz.
- Performans hedefi (M2 sonu): 2 yıl NQ 1m üzerinde `BarFill` ile tek genom değerlendirmesi < 200 ms; `criterion` benchmark'ı CI'da.
- Her taş sonunda `CHANGELOG.md` ve `GROK_OPS.md`'nin CLI bölümü güncellenir (Grok'un bilmesi gerekenler).

---

## 9) Eski sistemden dersler (paper-challenge, Ekim 2026)

1. Uyuyan ortamda çalışan izleyici olayları saatlerce geç kaydetti → tek, sürekli çalışan daemon + gap tespiti + geçmiş veriden telafi.
2. Kaynak kesintisi "rug/toplam kayıp" sayılabiliyordu → kesinti asla fiyat olayı değildir; tur iptal edilir, durum değişmez.
3. Durum dışa aktarılırken atomik yazımla yarış oldu → dışa aktarma tutarlı anlık görüntüden (SQLite backup API) yapılır.
4. Fazla INFO olayları sessizce düşürülüyordu → hiçbir olay sessizce düşmez.
5. Stop sonrası aynı yöne hemen yeniden giriş ve korelasyonlu zıt pozisyonlar (EURGBP etkisi) → cooldown ve net maruziyet kontrolü `risk`/`portfolio`'da birinci sınıf.
6. Haftalık kapanışa yakın girişler hedefe ulaşamadan kapandı → seans sonu/hafta sonu kalan süre `Ctx`'te; genomlar bunu filtreleyebilir.
7. Global durum ve monkeypatch kırılgandı → global durum yok, bağımlılıklar trait ile enjekte edilir.
8. Az örnekle (n=9) sonuç çıkarıldı → gauntlet kapı 1 ve DSR bunu yapısal olarak engeller.
