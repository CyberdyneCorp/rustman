//! The process-wide wasmtime engine, its epoch ticker and the linker of
//! the host imports (design D4).

use std::sync::OnceLock;
use std::time::Duration;

use wasmtime::component::{HasSelf, Linker};
use wasmtime::{Config, Engine};

use super::HostState;
use crate::bindings::host::Host_;

/// How often the engine's epoch advances: the granularity of CPU limits.
pub const TICK: Duration = Duration::from_millis(10);

/// The largest WebAssembly stack, in bytes; deeper recursion traps.
const MAX_WASM_STACK: usize = 1024 * 1024;

/// The engine every plugin is compiled for. The first call starts the
/// thread that advances its epoch every [`TICK`].
pub fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.epoch_interruption(true);
        config.max_wasm_stack(MAX_WASM_STACK);
        let engine = Engine::new(&config).expect("the plugin engine configuration is valid");
        let ticking = engine.clone();
        std::thread::Builder::new()
            .name("rustman-plugin-epoch".to_owned())
            .spawn(move || {
                loop {
                    std::thread::sleep(TICK);
                    ticking.increment_epoch();
                }
            })
            .expect("the plugin epoch thread starts");
        engine
    })
}

/// The linker of every host import, and nothing else.
pub fn linker() -> &'static Linker<HostState> {
    static LINKER: OnceLock<Linker<HostState>> = OnceLock::new();
    LINKER.get_or_init(|| {
        let mut linker = Linker::new(engine());
        Host_::add_to_linker::<HostState, HasSelf<HostState>>(&mut linker, |state| state)
            .expect("the host imports link once");
        linker
    })
}
