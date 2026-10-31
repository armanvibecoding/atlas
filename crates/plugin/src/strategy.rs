//! `PluginStrategy`: sandbox'taki modülü `atlas_core::Strategy` olarak sunar.

use crate::abi::{self, PluginInput};
use crate::host::{CallError, PluginModule, PluginRunner};
use crate::{PluginError, PluginLimits};
use atlas_core::{Ctx, InstrumentId, Intent, MarketEvent, MarketPayload, Strategy, StrategyId};

/// Çağrı ve hata sayaçları. Devre dışı kalma kararı `failures()` üzerinden verilir.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginStats {
    /// Plugin'in çağrıldığı bar olayı sayısı.
    pub calls: u64,
    /// Geçerli çıktıyla biten çağrılar.
    pub ok: u64,
    pub out_of_fuel: u64,
    /// Çalışma zamanı tuzakları (bellek limiti, `unreachable`, sınır dışı erişim, yeniden instantiate).
    pub traps: u64,
    /// ABI ihlalleri (geçersiz adres, çok büyük çıktı).
    pub abi_errors: u64,
    /// Şemaya/doğrulamaya uymayan çıktılar (atıldı).
    pub invalid_outputs: u64,
    pub disabled: bool,
    pub last_error: Option<String>,
}

impl PluginStats {
    pub fn failures(&self) -> u64 {
        self.out_of_fuel + self.traps + self.abi_errors + self.invalid_outputs
    }
}

/// WASM plugin stratejisi. Yalnız kendi enstrümanının bar olaylarında plugin'i çağırır.
pub struct PluginStrategy {
    id: StrategyId,
    inst: InstrumentId,
    runner: PluginRunner,
    stats: PluginStats,
}

impl PluginStrategy {
    /// Kimlik host tarafından atanır: `plugin:<ad>#<hash ilk 10>`.
    pub fn new(
        name: &str,
        module: PluginModule,
        inst: InstrumentId,
        limits: PluginLimits,
    ) -> Result<Self, PluginError> {
        let id = StrategyId::new(format!(
            "plugin:{name}#{}",
            module.hash().get(..10).unwrap_or(module.hash())
        ));
        Ok(PluginStrategy {
            id,
            inst,
            runner: PluginRunner::new(module, limits)?,
            stats: PluginStats::default(),
        })
    }

    pub fn stats(&self) -> &PluginStats {
        &self.stats
    }

    pub fn is_disabled(&self) -> bool {
        self.stats.disabled
    }

    pub fn module(&self) -> &PluginModule {
        self.runner.module()
    }

    fn fail(&mut self, msg: String) {
        self.stats.last_error = Some(msg);
        if self.stats.failures() >= self.runner.limits().max_failures {
            self.stats.disabled = true;
        }
    }
}

impl Strategy for PluginStrategy {
    fn id(&self) -> &StrategyId {
        &self.id
    }

    fn on_event(&mut self, ctx: &Ctx<'_>, ev: &MarketEvent) -> Vec<Intent> {
        if self.stats.disabled || ev.instrument != self.inst {
            return Vec::new();
        }
        let MarketPayload::Bar(bar) = &ev.payload else {
            return Vec::new();
        };
        // Zaman noktalı güvence ve kesinti: kesinti fiyat olayı değildir (ders §9.2).
        if bar.close_ts > ctx.now() || ctx.feed_down(self.inst) {
            return Vec::new();
        }
        let limits = *self.runner.limits();
        let input = PluginInput::from_ctx(ctx, self.inst, limits.window_bars);
        let bytes = match serde_json::to_vec(&input) {
            Ok(b) => b,
            Err(e) => {
                self.stats.abi_errors += 1;
                self.fail(format!("girdi serileştirilemedi: {e}"));
                return Vec::new();
            }
        };
        self.stats.calls += 1;
        let out = match self.runner.call(&bytes) {
            Ok(o) => o,
            Err(e) => {
                match e {
                    CallError::OutOfFuel => self.stats.out_of_fuel += 1,
                    CallError::Trap(_) | CallError::Instantiate(_) => self.stats.traps += 1,
                    CallError::Abi(_) => self.stats.abi_errors += 1,
                }
                self.fail(e.to_string());
                return Vec::new();
            }
        };
        match abi::parse_output(&out, &input, &limits) {
            Ok(list) => {
                self.stats.ok += 1;
                let bar_ns = bar.close_ts.since(bar.open_ts);
                abi::to_intents(list, &self.id, self.inst, ctx.now(), bar_ns)
            }
            Err(e) => {
                self.stats.invalid_outputs += 1;
                self.fail(e.to_string());
                Vec::new()
            }
        }
    }
}
