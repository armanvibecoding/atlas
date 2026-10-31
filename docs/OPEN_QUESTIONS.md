# Açık sorular (Arman'a)

Belirsizlikte uydurma yok (spec §8). Her madde: durum, neden önemli, önerilen yol.
Son güncelleme: 2026-10-31 (M1).

## Kapananlar

### 1–2. Hedef firma ve prop config — **KAPANDI (2026-10-31)**
- Arman kararı: "genel prop firma kuralları olsun yeterli". Belirli firma yok.
- `configs/props/generic_100k.toml`: 100k, hedef %10, statik toplam zarar %10, günlük zarar %5
  (gün başı bakiye, UTC gece yarısı), 5 kontrat, hafta sonu tutma yok, haber ±2 dk, algo serbest.
- `verify m1` 24/24 yeşil.
- Not: gerçek bir firmaya geçerken (M5) o firmanın yazılı kuralları + Türkiye uygunluğu + algo izni ayrıca
  teyit edilir (Topstep/MFFU/Apex Türkiye'yi kabul etmiyor).

## Onay bekleyen öneriler

### 3. Atlas risk politikası (`configs/atlas.toml [risk]`)
Firma kurallarından ayrı, bizim sıkılaştırmalarımız. Önerilen başlangıç:
`cooldown 15 dk`, `işlem başı risk $250`, `grup net 2 kontrat`, `consistency tamponu %80`,
`drawdown yakınlık uyarısı %25`, `stop kayma tamponu 2 tick`, `hafta kapanışından 60 dk önce giriş yok`.
Çalışma anında yalnız sıkılaştırılabilir; gevşetme = senin onayın (§4.12).

### 4. CME kontrat spesifikasyonları ve maliyetleri
- Tick boyutu/değeri (NQ 0.25/$5, MNQ 0.25/$0.50, ES 0.25/$12.50, MES 0.25/$1.25, GC 0.10/$10,
  MGC 0.10/$1, CL 0.01/$10) CME spesifikasyonundan; teyit et.
- Komisyon ve borsa ücretleri **boş** (broker/firma bağımlı). Boşken bu enstrümanlarla simülasyon hata verir.

### 5. Binance maliyetleri ve para birimi
- `fee_rate_percent = 0.05` (VIP0 taker, yayımlanmış tablo) — teyit.
- PnL USDT cinsinden; `Money` USD kabul ediliyor (USDT≈USD varsayımı). Prop hesabı için önemli değil
  (kripto prop hedefi yok), araştırma için kabul edilebilir mi?

### 6. Kayma (`slippage_ticks`) boş
- Hiçbir enstrümanda kayma girilmedi (uydurulmaz; M4 kalibrasyonu ölçecek).
- M2 gauntlet'i (maliyet stresi, kapı 7) bir başlangıç kaymasına ihtiyaç duyacak.
- Öneri: Binance için `aggTrades`'ten ölçülmüş efektif spread'le başlamak (ücretsiz, ölçüm, uydurma değil).

## Bilgi / kayıt

### 7. CME tatil takvimi girilmedi
`configs/calendar.toml` mekanizması hazır (`[[session.closure]]`); resmi CME takviminden yıl yıl girilecek.
Girilmezse tatil boşlukları veri kalite monitöründe `gap` olarak görünür (sessiz değil).

### 8. Databento
Maliyet sorgusu + `data.max_usd_per_pull` kapısı hazır (`atlas data quote-databento`). Tavan boş = 0
(her ücretli çekim onay ister). Hesap açılırsa ($125 ücretsiz kredi, kart gerekir) DBN indirme eklenecek.
FTMO yoluna gidilirse CME verisi yalnız karşılaştırma içindir.

### 9. Dukascopy erişimi
`datafeed.dukascopy.com` iki denemede de bağlanmadı (ilki zaman aşımı, ikincisi SSL bağlantı hatası —
curl 35). Bu makinedeki ağ/VPN (WireSock kurulu) veya Dukascopy tarafı olabilir; VPS'ten denenecek. FTMO hedefi için XAUUSD / NAS100 CFD verisi
gerekecek. Sonraki adımda tekrar denenecek (LZMA `.bi5` çözümü için yeni bağımlılık → ADR).

### 10. Veri kalite bulgusu: BTCUSDT 2025-08-29
06:19–06:36 UTC arası 18 dakika **sıfır hacimli** bar (borsa tarafı). Veri değiştirilmedi; 18 `data_quality`
olayı journal'da. Stratejiler bu aralıkta işlem görmez (fiyat sabit).

### 11. Consistency tavanı, önceki kâr ≤ 0 iken
Kural `bugün ≤ r·(önceki + bugün)` ⇒ tavan `r·P/(1−r)`. `P ≤ 0` iken tavan tanımsız; sonraki günler
seyreltebileceği için kısıt uygulanmıyor. Firmanın kuralı ödeme anında mı, her gün mü değerlendiriyor —
config doldurulurken teyit.

### 12. Hafta sonu kapatma öncesi süre
Hafta sonu tutma yasaksa pozisyonlar haftalık kapanıştan **5 dk önce** piyasa emriyle kapanır (sabit,
`WEEKEND_FLATTEN_LEAD_MIN`). Yeni giriş yasağı politika ile (öneri 60 dk).
