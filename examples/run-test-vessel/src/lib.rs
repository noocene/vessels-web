use core::convert::Infallible;
use futures::task::{FutureObj, LocalFutureObj, LocalSpawn, Spawn, SpawnError};
use protocol_mve_transport::ProtocolMveTransport;
use std::panic;
use vessels::{
    register,
    resource::ResourceManagerExt,
    runtime::{CoalesceFramed, ModuleResource, Wasm},
    with_core, Convert, Core, MemoryStore, Ring, Sha256, SimpleResourceManager,
};
use vessels_web::WebRuntime;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen_futures::spawn_local;
use web_sys::console::log_1;

pub struct SpawnShim;

impl LocalSpawn for SpawnShim {
    fn spawn_local_obj(&self, future: LocalFutureObj<'static, ()>) -> Result<(), SpawnError> {
        spawn_local(future);
        Ok(())
    }
}

impl Spawn for SpawnShim {
    fn spawn_obj(&self, future: FutureObj<'static, ()>) -> Result<(), SpawnError> {
        spawn_local(future);
        Ok(())
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    panic::set_hook(Box::new(console_error_panic_hook::hook));

    spawn_local(entry());
}

async fn entry() {
    let core = Core::new();

    with_core! { &core => {
        let mut manager = SimpleResourceManager::new();

        let mut store = MemoryStore::<Sha256>::new();

        manager.add_provider(store.clone()).await;

        register(move || {
            let manager = manager.clone();

            Box::pin(async move { Ok::<_, Infallible>(manager.erase_resource_manager()) })
        })
        .await
        .unwrap();

        let resource: ModuleResource<String> = store
            .intern::<Ring, _, Convert>(Wasm(
                include_bytes!("../../../target/wasm32-unknown-unknown/debug/test_vessel.wasm").to_vec(),
            ))
            .await
            .unwrap().into();

        let mut runtime = WebRuntime;

        let data = runtime
            .coalesce_framed_local::<_, _, ProtocolMveTransport<_>>(SpawnShim, resource)
            .await
            .unwrap();

        log_1(&data.into());
    }};
}
