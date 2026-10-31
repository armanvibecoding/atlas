//! wasmtime sandbox'ı: motor ayarları, modül yükleme, instantiate ve tek çağrı.

use crate::{PluginError, PluginLimits};
use sha2::{Digest, Sha256};
use wasmtime::{
    Config, Engine, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, Trap, TypedFunc,
};

/// wasm yığın üst sınırı (derin özyineleme tuzağa düşer, host yığını taşmaz).
const MAX_WASM_STACK: usize = 512 << 10;

/// Deterministik, kesmesiz, yakıt sayan motor.
fn engine() -> Result<Engine, PluginError> {
    let mut c = Config::new();
    // Sonsuz döngü → yakıt biter → tuzak. Zaman tabanlı kesme yok (determinizm).
    c.consume_fuel(true);
    c.epoch_interruption(false);
    // Platformdan bağımsız NaN bit desenleri.
    c.cranelift_nan_canonicalization(true);
    // Sonucu donanıma bağlı olabilen öneriler kapalı.
    c.wasm_relaxed_simd(false);
    c.relaxed_simd_deterministic(true);
    // Thread önerisi: wasmtime `threads` özelliği derlenmedi, paylaşılan bellek hiç desteklenmez.
    c.wasm_multi_memory(false);
    c.wasm_memory64(false);
    c.max_wasm_stack(MAX_WASM_STACK);
    Engine::new(&c).map_err(|e| PluginError::Engine(e.to_string()))
}

/// Derlenmiş ve import denetiminden geçmiş plugin modülü.
#[derive(Clone)]
pub struct PluginModule {
    engine: Engine,
    module: Module,
    hash: String,
}

impl std::fmt::Debug for PluginModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginModule")
            .field("hash", &self.hash)
            .finish_non_exhaustive()
    }
}

impl PluginModule {
    /// wasm ikili (ya da WAT metni) baytlarından derler. Herhangi bir import → `ForbiddenImport`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PluginError> {
        let module = Self::compile_unchecked(bytes)?;
        if let Some(imp) = module.imports().next() {
            return Err(PluginError::ForbiddenImport {
                module: imp.module().to_string(),
                name: imp.name().to_string(),
            });
        }
        let hash = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(PluginModule {
            engine: module.engine().clone(),
            module,
            hash,
        })
    }

    /// `.wasm` dosyasından yükler.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, PluginError> {
        let p = path.as_ref();
        let bytes =
            std::fs::read(p).map_err(|e| PluginError::Io(format!("{}: {e}", p.display())))?;
        Self::from_bytes(&bytes)
    }

    fn compile_unchecked(bytes: &[u8]) -> Result<Module, PluginError> {
        Module::new(&engine()?, bytes).map_err(|e| PluginError::Compile(format!("{e:#}")))
    }

    /// Yüklenen baytların SHA-256'sı (hex). Sicildeki "plugin hash" budur (§4.7).
    pub fn hash(&self) -> &str {
        &self.hash
    }
}

/// Bir çağrının hatası. Hepsi "o olayda intent yok" demektir; sayılır (bkz. `PluginStats`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    #[error("yakıt bitti")]
    OutOfFuel,
    #[error("tuzak: {0}")]
    Trap(String),
    #[error("ABI ihlali: {0}")]
    Abi(String),
    #[error("yeniden instantiate edilemedi: {0}")]
    Instantiate(String),
}

struct HostState {
    limits: StoreLimits,
}

struct Live {
    store: Store<HostState>,
    memory: Memory,
    alloc: TypedFunc<i32, i32>,
    on_event: TypedFunc<(i32, i32), i64>,
}

fn instantiate(m: &PluginModule, l: &PluginLimits) -> Result<Live, PluginError> {
    let limits = StoreLimitsBuilder::new()
        .memory_size(l.max_memory_bytes)
        .memories(1)
        .tables(4)
        .table_elements(l.max_table_elements)
        .instances(1)
        .trap_on_grow_failure(true)
        .build();
    let mut store = Store::new(&m.engine, HostState { limits });
    store.limiter(|s| &mut s.limits);
    // `start` fonksiyonu da yakıtla sınırlıdır.
    store
        .set_fuel(l.fuel_per_event)
        .map_err(|e| PluginError::Engine(e.to_string()))?;
    // Boş linker: hiçbir host fonksiyonu tanımlı değil; import isteyen modül burada da düşer.
    let linker: Linker<HostState> = Linker::new(&m.engine);
    let instance = linker
        .instantiate(&mut store, &m.module)
        .map_err(|e| PluginError::Instantiate(format!("{e:#}")))?;
    let memory = instance
        .get_memory(&mut store, "memory")
        .ok_or_else(|| PluginError::Abi("`memory` dışa aktarılmamış".into()))?;
    let alloc = instance
        .get_typed_func::<i32, i32>(&mut store, "alloc")
        .map_err(|e| PluginError::Abi(format!("`alloc(i32) -> i32` yok: {e}")))?;
    let on_event = instance
        .get_typed_func::<(i32, i32), i64>(&mut store, "on_event")
        .map_err(|e| PluginError::Abi(format!("`on_event(i32, i32) -> i64` yok: {e}")))?;
    Ok(Live {
        store,
        memory,
        alloc,
        on_event,
    })
}

fn classify(e: &wasmtime::Error) -> CallError {
    match e.downcast_ref::<Trap>() {
        Some(Trap::OutOfFuel) => CallError::OutOfFuel,
        _ => CallError::Trap(format!("{e:#}")),
    }
}

/// Çalışan plugin örneği. Örnek çağrılar arasında yaşar (plugin kendi durumunu tutabilir); herhangi bir
/// hata sonrası atılır ve sonraki çağrıda temiz bir örnek kurulur. Girdi dizisi aynıysa çıktı dizisi aynıdır.
pub struct PluginRunner {
    module: PluginModule,
    limits: PluginLimits,
    live: Option<Live>,
    last_fuel_used: u64,
}

impl PluginRunner {
    /// Instantiate eder ve ABI'yi doğrular (export'lar ve imzaları).
    pub fn new(module: PluginModule, limits: PluginLimits) -> Result<Self, PluginError> {
        let live = instantiate(&module, &limits)?;
        Ok(PluginRunner {
            module,
            limits,
            live: Some(live),
            last_fuel_used: 0,
        })
    }

    pub fn module(&self) -> &PluginModule {
        &self.module
    }

    pub fn limits(&self) -> &PluginLimits {
        &self.limits
    }

    /// Son başarılı çağrının harcadığı yakıt (determinizm göstergesi).
    pub fn last_fuel_used(&self) -> u64 {
        self.last_fuel_used
    }

    /// Bir çağrı: girdiyi yazar, `on_event`'i çalıştırır, çıktı baytlarını kopyalar.
    pub fn call(&mut self, input: &[u8]) -> Result<Vec<u8>, CallError> {
        let mut live = match self.live.take() {
            Some(l) => l,
            None => instantiate(&self.module, &self.limits)
                .map_err(|e| CallError::Instantiate(e.to_string()))?,
        };
        let out = run(&mut live, input, &self.limits)?;
        self.last_fuel_used = self
            .limits
            .fuel_per_event
            .saturating_sub(live.store.get_fuel().unwrap_or(0));
        // Yalnız başarılı çağrıdan sonra örnek korunur; hata durumunda `live` düşer.
        self.live = Some(live);
        Ok(out)
    }
}

fn run(live: &mut Live, input: &[u8], l: &PluginLimits) -> Result<Vec<u8>, CallError> {
    let len = i32::try_from(input.len()).map_err(|_| CallError::Abi("girdi çok büyük".into()))?;
    live.store
        .set_fuel(l.fuel_per_event)
        .map_err(|e| CallError::Trap(e.to_string()))?;
    let ptr = live
        .alloc
        .call(&mut live.store, len)
        .map_err(|e| classify(&e))?;
    live.memory
        .write(&mut live.store, ptr as u32 as usize, input)
        .map_err(|_| CallError::Abi(format!("alloc geçersiz adres döndürdü: {ptr}")))?;
    let r = live
        .on_event
        .call(&mut live.store, (ptr, len))
        .map_err(|e| classify(&e))?;
    let r = r as u64;
    let optr = (r >> 32) as usize;
    let olen = (r & 0xFFFF_FFFF) as usize;
    if olen == 0 {
        return Ok(Vec::new());
    }
    if olen > l.max_output_bytes {
        return Err(CallError::Abi(format!(
            "çıktı çok büyük: {olen} > {}",
            l.max_output_bytes
        )));
    }
    let end = optr
        .checked_add(olen)
        .ok_or_else(|| CallError::Abi("çıktı adresi taştı".into()))?;
    live.memory
        .data(&live.store)
        .get(optr..end)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| CallError::Abi(format!("çıktı bellek dışında: {optr}+{olen}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Statik import denetimi atlansa bile boş linker importlu modülü instantiate etmez.
    #[test]
    fn empty_linker_rejects_imports_even_without_static_check() {
        let wat = r#"(module
            (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (func (export "alloc") (param i32) (result i32) i32.const 0)
            (func (export "on_event") (param i32 i32) (result i64) i64.const 0))"#;
        let module = PluginModule::compile_unchecked(wat.as_bytes()).unwrap();
        let pm = PluginModule {
            engine: module.engine().clone(),
            module,
            hash: String::new(),
        };
        let e = instantiate(&pm, &PluginLimits::default()).err().unwrap();
        assert!(matches!(e, PluginError::Instantiate(_)), "{e}");
    }
}
