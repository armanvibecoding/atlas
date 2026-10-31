# GROK_OPS — Grok Bot işletim kılavuzu

> Okuyucu: Grok Bot (operatör). Sahibi: Arman. Yapım (M1–M4) tamamlandı; **devir §7.1 koşullarına bağlı**
> (VPS'te 7 günlük kesintisiz deneme, webhook bağlantısı, Arman onayı). O zamana kadar Grok sisteme dokunmaz.
> Son güncelleme: 2026-10-31 (M4 sonu).

## 1. Rol ve sınırlar

- Claude Code inşa eder, **Grok işletir**, Arman onaylar. Grok kod yazmaz.
- Tek erişim: SSH + `atlas-admin` zorunlu komut sarmalayıcısı → `atlas` CLI (`ATLAS_ROLE=grok`).
- Grok'un hiçbir komutu riski artıramaz, terfi ettiremez, sermaye değiştiremez. Bu iki katmanda uygulanır:
  sarmalayıcı beyaz listesi + CLI rol kontrolü (`verify m3` → `grok_permissions` testi).
- Reddedilen komut: çıkış kodu **126** (sarmalayıcı) ya da **2** + "izin yok" (CLI).

## 2. Komutlar

Her komut `--json` destekler; şema `docs/cli-schema.md` (`atlas.cli/1`). Çıkış: 0 iyi, 1 olumsuz sonuç, 2 hata.

| Komut | İş | Grok |
|---|---|---|
| `atlas status` | durum: pozisyonlar, kill switch'ler, prop config, veri | ✓ |
| `atlas health` | sağlık; çıkış 0 = sağlıklı | ✓ |
| `atlas report daily` / `weekly` | raporcu çıktısı + metrikler | ✓ |
| `atlas events [--since RFC3339\|seq:N] [--prio high\|medium\|info] [--kind K] [--limit N]` | olay kuyruğu | ✓ |
| `atlas strategies list` / `show <ad>` | merdivendeki stratejiler + son gauntlet raporu | ✓ |
| `atlas strategy pause <ad\|hash> <gerekçe>` | duraklat (yalnız kısıtlayıcı) | ✓ |
| `atlas strategy demote <ad\|hash> <gerekçe>` | bir basamak düşür (duraklatılmışsa duraklatma korunur) | ✓ |
| `atlas strategy resume <ad\|hash> <gerekçe>` | **yalnız Grok'un kendi duraklattığını** açar | ✓ |
| `atlas kill <global\|account\|strategy:ID> <gerekçe>` | kill switch: pozisyonlar kapanır | ✓ |
| `atlas note <metin>` | journal'a operatör notu | ✓ |
| `atlas experiments summary` | sicil: toplam deneme, kaynak/kapı dağılımı | ✓ |
| `atlas budget` | LLM / veri / arama bütçesi | ✓ |
| `atlas approvals list` / `show <id>` | bekleyen onay paketleri | ✓ |
| `atlas verify m1..m4` | kilometre taşı kabul testleri (m2 ~2 dk, m3 ~20 sn) | ✓ |
| `atlas approve <id> [--reject]` | onay | ✗ yalnız Arman (anahtar) |
| `atlas kill-reset <scope>` | kill switch'i aç | ✗ yalnız Arman |
| `atlas gauntlet run\|eval`, `data pull-binance\|quote-databento\|measure-slippage`, `journal checkpoint`, `daemon run`, `risk …`, `deploy`, `--home` | — | ✗ |

## 3. Olaylar ve uyandırma

- **HIGH** (uyandırır, sessiz saatte de): `kill_switch, risk_breach, daily_loss_limit, prop_drawdown_near,
  consistency_headroom, feed_down, engine_failure, clock_skew, drift_alarm, approval_request, budget_exhausted, outbox_dead`
- **MEDIUM** (sessiz saat dışında uyandırır): `position_opened, position_closed, promotion, demotion, gauntlet_pass,
  data_quality, fill_divergence`
- **INFO**: diğerleri (`heartbeat`, `experiment_done`, `search_cycle`, `shadow_signal`, `agent_call`, `stage_change`…).
  Tek başına uyandırmaz, **asla düşmez**: 09:00 TSİ gece özetinde gelir.
- Sessiz saatler 23:30–09:00 TSİ; saatte en fazla 6 uyandırma (aşan olaylar tek toplu mesajda); aynı anahtar
  30 dk içinde tekrar uyandırmaz.

### Webhook biçimi (`atlas.webhook/1`)

`POST <url>`, başlık `Idempotency-Key: <anahtar>` (aynı anahtarı iki kez işleme).

```json
{"schema": "atlas.webhook/1", "kind": "wake" | "wake_batch" | "digest", "created": <ns>, "body": {...}}
```
- `wake`: `body = {seq, time, kind, priority, key, payload}`
- `wake_batch`: `body = {note, events: [...]}`
- `digest`: `body = {day, events, by_kind, notable: [...], from_seq, to_seq}`

Teslim edilemezse üstel geri çekilme (30 sn → 1 sa), 24 denemeden sonra ölü kutusu + `outbox_dead` (HIGH).

## 4. Terfi merdiveni

`aday → gauntlet → gölge → paper → mikro eval → ölçek`
- Gauntlet'i 10/10 geçen → otomatik **gölge** (canlı veri, emir yok; `shadow_signal` olayları).
- Gölgede SPRT "beklentiyle tutarlı" → otomatik **paper** (`promotion`).
- Paper'da SPRT tutarlı → **onay talebi** (`approval_request`, HIGH). Mikro eval ve ölçek **yalnız Arman onayıyla**.
- Sapma (SPRT ya da CUSUM alarmı) → otomatik **bir basamak düşür** (`demotion`).

## 5. Grok ne yapmalı (öneri akışı)

| Olay | Grok'un yapacağı |
|---|---|
| `kill_switch`, `risk_breach`, `daily_loss_limit` | `status` + `events --prio high`; Arman'a özet; gerekirse `note` |
| `prop_drawdown_near`, `consistency_headroom` | ilgili stratejiyi `strategy pause` ile durdurmayı düşün; Arman'a bildir |
| `feed_down` (5 dk+) | `health`; sürerse Arman'a bildir (daemon boşluğu kendisi telafi eder) |
| `engine_failure` (saatte ≥3) | `strategy pause` + Arman'a bildir |
| `drift_alarm`, `demotion` | bilgi; tekrar ediyorsa Arman'a özet |
| `approval_request` | `approvals show <id>` → Arman'a özetle sun (karar Arman'ın) |
| `outbox_dead` | Grok'a ulaşmayan mesaj var: `events --since seq:N` ile eksikleri oku |
| gece özeti | `report daily` ile birlikte Türkçe özet |

## 6. Durum (2026-10-31)

- M1–M4 tamam: `verify m1` 24/24, `m2` 10/10, `m3` 5/5, `m4` 4/4; 219 test.
- Veri: Binance BTCUSDT + ETHUSDT 1m (2024-11 → 2026-10). Prop: genel 100k kuralları.
- Canlıda strateji yok: 5 referans strateji gauntlet kapı 2'de öldü (1m BTC'de taker ücreti edge'i yiyor).
- Ajanlar KAPALI (anahtar/fiyat/bütçe yok). Webhook adresi boş. VPS kurulumu yapılmadı.
- Devir için kalanlar: `HANDOFF.md`.
