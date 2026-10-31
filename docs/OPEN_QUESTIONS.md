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

### 3. Atlas risk politikası — **KAPANDI (2026-10-31)**
Arman: "%1 risk iyidir" → işlem başı en fazla $1.000 (100k'nın %1'i). Diğer öneriler itirazsız kabul:
stop sonrası 15 dk aynı yöne giriş yok, grup net 2 kontrat, consistency tamponu %80, drawdown yakınlık
uyarısı %25, stop kayma tamponu 2 tick, hafta kapanışından 60 dk önce yeni giriş yok.

### 4. CME kontrat spesifikasyonları ve maliyetleri — **KAPANDI (internetten teyit, 2026-10-31)**
- Tick: NQ 0.25/$5, MNQ 0.25/$0.50, ES 0.25/$12.50, MES 0.25/$1.25, GC 0.10/$10, MGC 0.10/$1, CL 0.01/$10
  — CME sayfaları + birden çok aracı kurum kaynağı aynı değerleri veriyor.
- Maliyet (tek yön, kontrat başı) = aracı komisyonu + takas + borsa + NFA, genel perakende modeli:
  NQ/ES $2.88, MNQ/MES $0.95, GC $3.15, MGC $1.30, CL $2.92. Kaynaklar `configs/instruments.toml` başında.
- Zayıf nokta: CL borsa ücreti tek kaynaklı (NexusFi tahmini); CME Fee Finder'dan teyit edilecek.

### 5. Binance maliyetleri — **KAPANDI (2026-10-31)**
VIP 0 taker %0.05 / maker %0.02, Binance resmi destek makalesiyle teyit. Tüm dolumlar taker sayılıyor
(karamsar). 1 USDT = 1 USD kabulü kaldı (araştırma için yeterli).

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

## M2'de eklenenler

### 13. Prop Monte Carlo eşikleri (kapı 9) — **onay bekliyor**
Spec: "eşikler Arman'ın". Öneri (`atlas.toml [gauntlet]`): geçme olasılığı ≥ **%50**, patlama olasılığı ≤ **%20**,
120 işlem günü ufkunda, genel 100k kuralları ile. MC gün içi açık-pozisyon dalgalanmasını görmez (iyimser);
eşikler bunu telafi edecek kadar sıkı tutuldu.

### 14. Journal ileri uyumluluğu
Yeni bir olay türü (ör. `calibration`) yazıldıktan sonra **eski** binary journal'ı açamaz (bilinmeyen tür = bozuk
kayıt sayılır, sessiz atlama yok). Kural: deploy'da önce yeni binary, sonra yeni olay türleri. M3 deploy betiği bunu uygular.

### 15. Ücret bulgusu (karar değil, bilgi)
1m BTC'de 1–1.5 ATR stop ≈ %0.12 fiyat hareketi; gidiş-dönüş taker ücreti %0.10 → riskin ~%85'i. Referans
stratejilerin üçü maliyetsiz pozitif ama maliyetle −1.4R…−2.2R. M3 araması bu yüzden (a) daha geniş stop/uzun
vade, (b) limit emir (maker %0.02) girişlerini öncelemeli. Sahte strateji testi gerçek maliyetle "kolay"
(gürültü ücretle ölüyor); asıl ölçü sürtünmesiz varyant (o da 0/1000).

### 16. Spec sapması: performans ölçümü
Spec "2 yıl NQ 1m < 200 ms" diyor; NQ verisi yok (M0). Ölçüm BTCUSDT 921 bin barda yapıldı (~215 ms) ve NQ 2 yıl
bar sayısına (≈717 bin) oranlandı (~167 ms). NQ verisi gelince doğrudan ölçülecek.

## M3–M4'te eklenenler

### 17. LLM ajanları — **karar bekliyor**
Sağlayıcı (Anthropic / xAI), model, 1M token fiyatı ve günlük USD tavanı `atlas.toml [agents]`'a; anahtar
`/etc/atlas/atlas.env`'e. Doldurulmadan hiçbir çağrı yapılmaz (şu an kapalı).

### 18. VPS — **engel: devir**
§7.1: `verify m1..m4` VPS'te, 7 günlük kesintisiz gölge+paper koşusu, Grok'un SSH erişimi. `deploy/install.sh` hazır
(Debian/Ubuntu). Önceki Hetzner VPS (mt5-bot) kaldırıldı; yeni sunucu ya da paper-challenge sunucusu?

### 19. Grok webhook adresi — **engel: devir**
`atlas.toml [webhook] url`. Boşken olaylar diskte bekler (kaybolmaz); `health` uyarır. Biçim: GROK_OPS.md §3.

### 20. Onay anahtarı
`atlas approve` için Arman'ın gizli anahtarının SHA-256'sı `atlas.toml [approvals] key_sha256`'ya; anahtar yalnız
Arman'da (`ATLAS_APPROVER_KEY`). Boşken onay komutu kapalı.

### 21. Devir onayı (`approve handoff`)
Spec `atlas approve handoff` diyor; CLI'da onaylar sayısal talep kimliğiyle. Devir koşulları sağlanınca daemon
`handoff` türünde talep açacak şekilde bağlanacak (küçük iş; VPS koşusuyla birlikte).

### 22. Gerçek dolumla kalibrasyon
M4 kalibrasyon kabulü sentetik gözlemle geçti. Gerçek paper/canlı dolum ancak gerçek borsa executor'ıyla (M5) gelir.

### 23. ML ve plugin henüz aramaya bağlı değil
Altyapı ve testler hazır; arama döngüsü şimdilik genomları kullanıyor. Meta-model sarmalı bir aday yine gauntlet'ten
geçmeli (sicile yazılır).

