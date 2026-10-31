# HANDOFF — Atlas devir durumu

> Spec §7.1 madde 6. Okuyucu: Arman ve Grok Bot. Hazırlayan: Claude Code. Tarih: 2026-10-31.
> **Devir henüz yapılmadı.** Aşağıdaki koşullar sağlanınca Arman devri onaylar (spec: `atlas approve handoff`;
> CLI'da `handoff` türünde onay talebi olarak açılacak — OPEN_QUESTIONS #21).

## 1. Sistemin durumu

| Taş | Kabul | Sonuç (yerel, Windows) |
|---|---|---|
| M1 Temel | rastgele/al-tut testleri, prop uyumu, determinizm, journal replay | `verify m1` **24/24** |
| M2 Doğruluk makinesi | sahte strateji ≤%1, 5 referans raporu | `verify m2` **10/10** (1000 sahte → 0 geçen; sürtünmesiz de 0) |
| M3 Otonomi | 72 sa insansız, deney üretir/eler, 0 outbox_dead, Grok izinleri | `verify m3` **5/5** (72 sa sanal saatle; gerçek VPS koşusu bekliyor) |
| M4 Derinlik | kalibrasyon farkı küçülür, sandbox reddi, dashboard tam | `verify m4` **4/4** |

219 test, clippy `-D warnings` temiz, GitHub CI yeşil. Repo: https://github.com/armanvibecoding/atlas

## 2. Devir koşulları (§7.1) — kalan

| # | Koşul | Durum |
|---|---|---|
| 1 | `verify m1..m4` **VPS üzerinde** yeşil | ⏳ VPS yok (yerelde yeşil) |
| 2 | 7 günlük kesintisiz gölge+paper denemesi | ⏳ VPS gerekli |
| 3 | Grok komutları VPS'te `atlas-admin` üzerinden; izin testleri yeşil | ⏳ VPS gerekli (yerelde yeşil) |
| 4 | Webhook Grok'un alarm routine'ine bağlı, test olayı teslim | ⏳ URL yok |
| 5 | `GROK_OPS.md` güncel | ✓ |
| 6 | `HANDOFF.md` | ✓ (bu belge) |

## 3. Canlıdaki stratejiler

Yok. Beş referans strateji (trend_pullback, sweep_mss, vwap_mean_reversion, opening_range_breakout,
cvd_divergence) gauntlet kapı 2'de (CPCV) öldü. Maliyetsiz üçü pozitif (+0.03…+0.06R/işlem) ama gerçek taker
ücreti işlem başı ~0.85–1.3R. Arama (daemon) bu yüzden geniş stop / uzun vade / limit giriş yönünde çalışmalı.

## 4. Bütçeler

- LLM: **kapalı** (`atlas.toml [agents]` sağlayıcı/fiyat/günlük USD boş ya da 0). Kapalıyken hiçbir çağrı yapılmaz.
- Veri: Databento tavanı 0 (ücretli çekim onaysız yapılamaz). Binance ücretsiz.
- Arama: dakika başına 4 değerlendirme; her tarama sicilde deneme sayılır (DSR'ye girer).

## 5. Bilinen sınırlamalar

- **Paper = simüle borsa.** Gerçek borsa executor'ı yok (M5). Kalibrasyon kabulü sentetik gözlemle
  geçti; gerçek paper/canlı dolumla ölçüm M5'te.
- **Kuyruk modeli L1 tabanlı** (MBO verisi yok). Kısmi dolum yok.
- **Prop MC** gün içi açık-pozisyon dalgalanmasını görmez (iyimser); eşikler (%50/%20) öneri.
- **Kitap derinliği feature'ı yok** (L2 kaydı yok).
- **NQ performans hedefi** BTC verisinde ölçülüp bar sayısıyla ölçeklendi (NQ verisi yok).
- Meta-model (ML) ve plugin altyapısı hazır ama henüz arama döngüsüne bağlı değil (genom yolu aktif).
- Journal ileri uyumluluğu: yeni olay türü yazıldıktan sonra eski binary journal'ı açamaz → deploy sırası:
  önce binary.

## 6. İlk hafta Grok'un dikkat etmesi gerekenler

1. `health` her sabah; `report daily` gece özetiyle birlikte.
2. `feed_down` art arda gelirse (Binance erişimi): daemon boşluğu telafi eder; 30 dk+ sürerse Arman'a.
3. `experiments summary`: deneme sayısı artmalı (arama çalışıyor mu?); `outbox` ölü kutusu 0 kalmalı.
4. İlk `gauntlet_pass` ve gölge terfisi gelirse: `strategies show <ad>` raporunu Arman'a özetle.
5. `approval_request` → asla kendin karar verme; Arman'a sun.

## 7. Açık sorular

`docs/OPEN_QUESTIONS.md` (1–5 kapandı; 6–21 açık/bilgi).
