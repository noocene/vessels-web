use core::{
    convert::{Infallible, TryFrom},
    pin::Pin,
    task::{Context, Poll},
};
use core_futures_io::AsyncWrite;
use std::{fs::read, string::FromUtf8Error};
use vessels::{
    register,
    resource::ResourceManagerExt,
    runtime::{Runtime, Wasm},
    with_core, Convert, Core, MemoryStore, Ring, Sha256, SimpleResourceManager,
};
use wasm_bindgen_futures::spawn_local;
use vessels_web::WebRuntime;
use wasm_bindgen::prelude::wasm_bindgen;
use std::panic;
use web_sys::console::log_1;

#[derive(Debug, Clone)]
pub struct Tester(String);

impl TryFrom<Vec<u8>> for Tester {
    type Error = FromUtf8Error;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        String::from_utf8(value).map(Tester)
    }
}

impl From<Tester> for Vec<u8> {
    fn from(tester: Tester) -> Vec<u8> {
        tester.0.as_bytes().into()
    }
}

pub struct TestWriter;

impl AsyncWrite for TestWriter {
    type WriteError = Infallible;
    type FlushError = Infallible;
    type CloseError = Infallible;

    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context,
        buf: &[u8],
    ) -> Poll<Result<usize, Self::WriteError>> {
        log_1(&format!("got data {:?}", buf).into());
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context) -> Poll<Result<(), Self::FlushError>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context) -> Poll<Result<(), Self::CloseError>> {
        Poll::Ready(Ok(()))
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

        let resource = store
            .intern::<Ring, _, Convert>(Wasm(
                include_bytes!("../../../target/wasm32-unknown-unknown/debug/test_vessel.wasm").to_vec(),
            ))
            .await
            .unwrap();

        let mut runtime = WebRuntime;

        runtime
            .instantiate(resource, TestWriter, [10u8, 2u8, 3u8, 50u8].as_ref())
            .await
            .unwrap();
    }};
}
