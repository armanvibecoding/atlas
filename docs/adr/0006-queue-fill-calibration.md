# ADR 0006 — Kuyruk pozisyonlu dolum (`MboQueueFill`) ve dolum kalibrasyonu

- Durum: kabul (M4, 2026-10-31)
- Kod: `crates/sim/src/fill_queue.rs`, `crates/sim/src/calibration.rs`

## Bağlam

§4.4 `MboQueueFill` ister: limit emir, önündeki hacim tükenmeden dolmaz. Gecikme ve basit piyasa etkisi
de modellenmeli. Ayrıca paper/canlı dolumlar simülasyonla eşlenip kayma ve dolum olasılığı kalibre
edilmeli, her kalibrasyon sürüm olarak saklanmalı; deney hangi fill-model sürümünü kullandığını kaydeder.
M4 kabulü: paper ile simülasyon arasındaki kayma farkı ölçülür ve kalibrasyon sonrası küçülür.

**Kısıt:** gerçek MBO verisi yok (Databento hesabı M0'da bekliyor). Elimizde L1 kotasyon (`Quote`: en iyi
bid/ask + görünür boyut) ve işlem (`Trade`) akışı olabilir.

## Seçenekler

1. MBO gelene kadar beklemek — M4 kabulünü bloklar.
2. Kuyruğu olasılıksal (rastgele) modellemek — determinizm için seed yeterli olsa da kalibrasyonu ve
   hata ayıklamayı zorlaştırır.
3. **L1 + işlem akışıyla deterministik, karamsar kuyruk tahmini; MBO gelince aynı arayüzle kesinleşme.**

Seçilen: 3.

## Karar

### Kuyruk tahmini (L1)
Emir ilk görüldüğünde son kotasyona göre:

| Limit fiyatı | Önündeki kuyruk |
|---|---|
| Karşı en iyi fiyata eşit/daha iyi (pazarlanabilir) | Hemen, karşı en iyi fiyattan dolum |
| Kendi tarafın en iyisinden daha iyi | 0 |
| Kendi tarafın en iyisine eşit | O taraftaki görünür boyut |
| Kitabın içinde (daha kötü) ya da kotasyon yok | **Bilinmiyor** → yalnız içinden geçişle dolum |

Sonraki olaylar:
- Limit fiyatında, bizim tarafa vuran işlem (agresör karşı taraf ya da bilinmiyor) kuyruğu azaltır;
  kuyruk tükenince (kümülatif hacim ≥ önündeki) limit fiyatından dolum.
- İşlem limitin içinden geçerse ya da karşı en iyi fiyat limitin içine girerse: dolum (kuyruk ne olursa).
- Seviye en iyi fiyatken görünür boyut tahminden küçükse tahmin o boyuta iner (görünür boyut her zaman
  önümüzdeki kuyruğun üst sınırıdır). Bilinmeyen kuyruk, seviye en iyi olduğunda bu üst sınırla bilinir olur.
- Seviye tamamen kaybolursa (alışta bid < limit < ask) önümüzde kimse kalmamıştır: kuyruk 0.

Tahmin her zaman gerçek kuyruğun **üst sınırıdır** (karamsar); iptallerin önümüzde mi arkamızda mı
olduğunu L1 söylemez, bu yüzden yalnız görünür boyut düştüğünde ve o boyuta kadar indirilir.

**MBO gelince değişecek kısım:** yalnız kuyruk durumunun kaynağı. Emir kimlikleriyle önümüzdeki emirler
birebir sayılır (iptal/değişiklik önde mi arkada mı kesin), "bilinmeyen kuyruk" hâli ve görünür-boyut üst
sınırı kalkar, kısmi dolum mümkün olur. `FillModel` arayüzü, `QueueConfig`, kalibrasyon ve sürümleme
aynen kalır; sürüm adı değişmez, model sürümü artar.

### Piyasa/stop, etki, gecikme
- Piyasa emri: işlem fiyatı (kotasyon olayında karşı en iyi fiyat) + kayma + etki. Stop yalnız işlemle
  tetiklenir (CME stop'u işlem fiyatıyla tetikler).
- Etki: `ceil(impact_milli_ticks_per_lot × (adet − 1) / 1000)` tick. Binde tick seçildi çünkü lot başına
  tam tick (ör. MNQ) fazla kaba; f64 yerine tamsayı (kural 1). 1 lot etkisizdir; temel kayma onu kapsar.
- Gecikme: motorun mevcut `WorkingOrder::active_from` alanı (emir bu andan önceki olaylarla eşleşmez).
- Kotasyonsuz akışta model, `TradeFill` (TradeThrough) ile birebir aynı işlemleri üretir (test edildi).

### Trait değişikliği (geriye uyumlu)
`FillModel`'e iki varsayılan-boş metot eklendi: `observe(&self, ev)` (her olayda, eşleştirmeden önce;
son kotasyonu öğrenmek için) ve `order_closed(&self, id)` (iptal/OCO/süre dolumunda emrin durumunu
bırakmak için). `BarFill`/`TradeFill` davranışı değişmedi. Kuyruk durumu `RefCell` içinde (emir kimliği →
kuyruk); model **koşu başına** kullanılır (emir kimlikleri koşular arasında tekrarlanır). Ödünç alma
başarısız olursa dolum yok (karamsar), panik yok.

### Kalibrasyon
- Gözlem (`FillObservation`): enstrüman, yön, tür (piyasa/stop/limit), adet, referans fiyat, sim fiyatı,
  canlı fiyatı, canlı doldu mu. Limit gözlemleri simülasyonun "doldu" dediği emirlerdir.
- Ölçüm (`measure`): aleyhte kayma (canlı ve sim, ortalama/ortanca), ortalama sim−canlı, ortalama
  |sim−canlı| (M4 kabul ölçüsü), limit dolum oranı. Gözlem yoksa `None` — 0 değil.
- Güncelleme (`calibrate`), enstrüman başına:
  - kayma = ortanca(canlı aleyhte kayma − etki(adet)), **yukarı** yuvarlanır, ≥ 0;
  - limit dolum olasılığı = canlı dolum oranı, 0,01'e **aşağı** yuvarlanır (≥ 0,01). Modelde `p < 1`
    görünür kuyruğu `1/p` ile şişirir (deterministik);
  - gözlem `min_samples`'tan (varsayılan 10) azsa eski değer kalır; etki kalibre edilmez (adet çeşitliliği yok).
  - Ortanca seçildi: tekil büyük kaymalar (haber anı) kalibrasyonu sürüklemesin.

### Sürümleme
- `FillParams::version` her kalibrasyonda bir artar (veri yetersiz olsa da; deneme kayda geçer),
  `parent_version` zinciri tutulur. `FillParams::id()` = `queue@v3`; `MboQueueFill::name()/version()`
  aynı kimliği verir, motor `SimResult::fill_model`'e bunu yazar → deney kaydı (§4.7) fill-model sürümünü taşır.
- `CalibrationStore`: `<dizin>/<model>_v000003.json`, dizin parametre (varsayılan `var/calibration/`).
  Yazım yalnız ekleme: aynı sürüm varsa hata; atomik (geçici dosya + sabit bağlantı, hedef varsa başarısız).
  Eski sürümler hiç değişmez → geçmiş sonuçlar kendi sürümleriyle yeniden üretilebilir.

## Sonuçlar
- Sentetik kabul testi: gerçek kayma 3 tick (±1 gürültü), başlangıç parametresi 1 tick. Ortalama
  |sim−canlı| 1,96 → 0,66 tick; ortalama sim−canlı −1,96 → +0,04; kalibre kayma 3 tick; `queue@v2`.
- Yeni bağımlılık yok (`tempfile` zaten workspace dev-bağımlılığı; `sim`'e dev olarak eklendi).
- **Açık entegrasyon (motor, `engine.rs`):**
  1. `step` içinde her olayda, `match_event`'ten önce `self.model.observe(ev)` çağrılmalı.
  2. Emir dolum dışı kaldırıldığında (`expire_entries`, OCO kardeş iptali) `self.model.order_closed(id)`.
  3. Enstrüman kayması: `slippage_for` önce aktif `FillParams::slippage_for(symbol)`'a bakmalı, yoksa
     `instruments.toml`; `slippage_mult_milli` (maliyet stresi) bunun üstüne uygulanmaya devam eder.
  4. Sicil (`registry`): kalibrasyon sürümleri `fill_params` olarak kaydedilmeli; deney satırındaki
     fill-model alanı zaten `SimResult::fill_model`'den gelir.
  5. Paper yürütücü (`exec::Paper`) gerçek dolumlardan `FillObservation` üretmeli (sim karşılığı aynı
     olay akışında `MboQueueFill` ile).
- Sınırlamalar: kısmi dolum yok (kuyruk tükenince tüm emir dolar; adet > 1 için iyimser); aynı olay bir
  emir için iki kez `match_one`'a verilirse işlem iki kez düşülür (motor bunu yapmaz); `observe` bağlanana
  kadar son kotasyon yalnız aktif emir varken öğrenilir (eksik bilgi karamsar yöne düşer).
