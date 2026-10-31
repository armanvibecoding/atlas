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

### `journal replay | export <ad> | checkpoint`
- replay: `{"events", "checkpoints", "mismatches": [..], "final_hash"}`, tutarsızsa çıkış 1.
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
outbox_dead`. MEDIUM: `position_opened, position_closed, promotion, demotion, gauntlet_pass, data_quality,
fill_divergence`. INFO: `heartbeat, experiment_done, operator_note, fill, intent_rejected, feed_up, kill_reset,
state_checkpoint, config_loaded, data_pulled, day_rollover`.

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

Strateji adı yalnız `[A-Za-z0-9_-]` (≤64); dosya yolu yalnız Arman'ın `gauntlet run|eval` komutlarında.

