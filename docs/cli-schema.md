# `atlas` CLI çıktı şeması — `atlas.cli/1`

Her komut `--json` ile makine-okunur çıktı verir. Şema sürümlüdür; alan **kaldırma veya anlam değiştirme**
yeni sürüm (`atlas.cli/2`) gerektirir, alan **ekleme** gerektirmez.

## Zarf

```json
{"schema": "atlas.cli/1", "command": "<komut>", "ok": true, "data": { ... }}
```

Hata (çıkış kodu 2):

```json
{"schema": "atlas.cli/1", "ok": false, "error": "<mesaj>"}
```

## Çıkış kodları

| Kod | Anlam |
|---|---|
| 0 | başarılı / sağlıklı / yeşil |
| 1 | komut çalıştı ama sonuç olumsuz (health sorunlu, verify kırmızı, replay tutarsız, maliyet tavan üstü) |
| 2 | hata (config okunamadı, izin yok, ağ hatası…) |

## Ortam

| Değişken | Anlam |
|---|---|
| `ATLAS_HOME` | `configs/` içeren kök (varsayılan: çalışma dizini; `--home` ile de verilir) |
| `ATLAS_ROLE` | `grok` ise yalnız-Arman komutları reddedilir (sarmalayıcıya ek ikinci kontrol) |
| `ATLAS_DATABENTO_KEY` | Databento API anahtarı (repo'da değil; `/etc/atlas/atlas.env`) |

## Komutlar (M1)

### `status`
```json
data: {"time": "...Z", "journal": {"events": 42, "last": EVENT|null}, "positions": {..},
       "realized_net_usd": 0.0, "kills": {"<scope>": "<sebep>"}, "prop": {"file","loaded","firm"|"error"},
       "data": [{"dataset","bars","quality_issues","from","to"}], "milestone": "M1"}
```

### `health`
```json
data: {"healthy": bool, "checks": [{"name", "ok", "detail"}]}
```
Kontroller: `instruments`, `calendar`, `risk_policy`, `prop_config`, `journal` (replay tutarlı), `data_store`.

### `events [--since <RFC3339|seq:N>] [--prio high|medium|info] [--kind <tür>] [--limit N]`
```json
data: [EVENT, ...]
EVENT = {"seq", "id", "ts" (ns), "time" (RFC3339), "kind", "priority", "key", "payload"}
```
`--prio medium` = medium + high.

### `verify m1`
```json
data: {"milestone": "m1", "green": bool, "passed": n, "total": n,
       "checks": [{"name", "pass", "detail"}]}
```

### `journal replay | export <ad> | checkpoint | rebase --reason <metin> [--dry-run]`
- replay: `{"events", "checkpoints", "mismatches": [..], "superseded": [..], "final_hash"}`, tutarsızsa çıkış 1.
  `superseded`: sonraki doğrulanmış bir `checkpoint_rebase` çapasının kapattığı eski uyuşmazlıklar (bilgi; tutarlılığı bozmaz).
- rebase (yalnız Arman): eski uyuşmazlıkları "bayat yazıcı" modeliyle SHA-256 düzeyinde yeniden üretebiliyorsa
  `checkpoint_rebase` yazar → `{"written", "seq", "supersedes", "missed_events"}`; açıklanamayan biri varsa hiçbir şey yazmaz.
- export: tutarlı anlık görüntü (SQLite backup API) → `var/exports/<ad>`; ad yalnız `[A-Za-z0-9._-]`, var olan dosyaya yazmaz.
- Grok rolünde `--home` reddedilir.
- checkpoint: yalnız Arman.

### `data list | pull-binance --symbol S --from YYYY-AA --to YYYY-AA | quote-databento ...`
- pull/quote yalnız Arman.

### `kill <global|account|strategy:ID> <gerekçe…>`
`{"scope", "seq"}` ya da `{"scope", "already_active": true}`.

### `kill-reset <scope>` — yalnız Arman.

### `note <metin…>` (ya da stdin) → `{"seq"}`.

## Olay türleri ve öncelikler

`core::events` tek kaynaktır (spec §5). HIGH: `kill_switch, risk_breach, daily_loss_limit, prop_drawdown_near,
consistency_headroom, feed_down, engine_failure, clock_skew, drift_alarm, approval_request, budget_exhausted,
outbox_dead, price_alert` (ADR 0012: Grok'un kendi alarmı). MEDIUM: `position_opened, position_closed, promotion,
demotion, gauntlet_pass, data_quality, fill_divergence`. INFO: `heartbeat, experiment_done, operator_note, fill,
intent_rejected, feed_up, kill_reset, state_checkpoint, checkpoint_rebase, config_loaded, data_pulled, day_rollover, calibration,
shadow_signal, grok_order, grok_order_applied, grok_alert_set, grok_alert_cancelled, agent_call, agent_invalid_output,
search_cycle, stage_change`.

## Komutlar (M2–M4)

| Komut | `data` |
|---|---|
| `strategies list` | `[{name, instrument, hash, tags, stage, last_experiment}]` |
| `strategies show <ad>` | `{file, genome, hash, last_experiment, report_md}` |
| `experiments summary` | `{summary: {trials, passed, by_source, failed_gate_histogram, lessons, holdout_accesses}, recent}` |
| `gauntlet run <ad\|yol>` (Arman) | `{experiment, file, report, similar_dead_ideas}` |
| `report daily\|weekly` | `{kind, since, events, by_kind, high, new_trials, new_passed, total_trials, stages, pending_approvals, kills}` |
| `strategy pause\|demote\|resume <ad\|hash> <gerekçe>` | `{strategy, hash, stage, seq}` |
| `budget` | `{llm: {provider, model, daily_usd, spent_today_usd, enabled, ...}, data, search}` |
| `approvals list` / `show <id>` | `{pending, total}` / `{id, kind, subject, payload, created_ts, decision}` |
| `approve <id> [--reject]` (Arman) | onay kaydı |
| `daemon run [--replay-days N] [--max-ticks N]` (Arman/systemd) | `DaemonSummary` |
| `verify m1..m4` | `{milestone, green, passed, total, checks: [{name, pass, detail}]}` |
| `hypothesis submit <json>` (ya da stdin) | `{id, queued, trials_counted, daily_left, genome}` |
| `hypothesis list` | `{hypotheses: [{id, created_ts, idea, timeframe, variants, pending, experiments, passed, best}], daily_limit}` |

### Hipotez şeması (`hypothesis submit`)
```json
{
  "idea": "Londra açılışında Asya aralığının üstüne kapanış devam eder",   // 10..1000 karakter
  "source": "gözlem: 2025 BTC 15m grafikleri",                              // 3..300
  "timeframe": "15m",                                  // 1m | 5m | 15m | 30m | 1h | 4h
  "entry": {"long": {"cross_above": ["close", "asia_high"]}},   // genom koşul DSL'i (long/short)
  "exit": {"atr": 14, "stop_atr": 1.5, "target_r": 2.0},        // isteğe bağlı; boşsa bu değerler
  "session_utc": [7, 10],                               // isteğe bağlı, UTC [başlangıç, bitiş)
  "falsification": "300+ işlemde komisyon sonrası beklenti ≤ 0" // isteğe bağlı
}
```
Bilinmeyen alan reddedilir. Sayılar başlangıç noktasıdır: temel genom + her parametrenin ±%25 komşusu
(en fazla 17 aday) `operatör:grok` kaynağıyla kuyruğa girer, aramadan önce işlenir; her aday deneme sayılır.
Günde en fazla 5 hipotez; aynı genom ikinci kez girmez. Aday adları `hip-<8hex>-vNN`.

### Genom: `timeframe_min`
İsteğe bağlı (5, 15, 30, 60, 240). Feature'lar, ATR ve `exit.time_bars` o dilimin barlarıyla hesaplanır; 1m barlar
UTC hizalı kovalara toplanır (gauntlet ve canlı koşucu aynı toplayıcı). Boş = 1m, eski genom hash'leri değişmez.

Strateji adı yalnız `[A-Za-z0-9_-]` (≤64); dosya yolu yalnız Arman'ın `gauntlet run|eval` komutlarında.

## Grok alarmları (ADR 0012)

| Komut | `data` |
|---|---|
| `alert add (--above <fiyat> \| --below <fiyat> \| --session london\|ny) [--expiry-hours 1..168] [--symbol BTCUSDT] [not…]` | `{alert_id, status: "active", alert, active_count}` |
| `alert list` | `{active: [alarm], recent: [alarm], limits: {max_active, max_expiry_hours, default_expiry_hours, session_grace_min}}` |
| `alert cancel <id> [gerekçe…]` | `{alert_id, status: "cancelled", seq}` |

`alarm` = `{alert_id, symbol, condition: price_above|price_below|session_open, level?, session?, session_open?,
created, expires, note, status: active|triggered|cancelled|expired, triggered_bar?, cancel_reason?}` (zamanlar
RFC3339 UTC, fiyat ondalık metin). Ret (çıkış 2): koşul sayısı ≠ 1, fiyat son fiyatın yanlış tarafında ya da son fiyat
yok, süre sınır dışı, seans alarmında süre, 20 etkin alarm, sembol daemon'un sembolü değil, alarm yok/etkin değil.

`price_alert` olay yükü: alarm alanları + `bar_ts` (ns), `bar_time`, `bar {open, high, low, close}`, `message`,
`account: "grok"`; anahtar `price_alert:<id>`.

