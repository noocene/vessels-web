use core_futures_io::{AsyncRead, AsyncWrite};
use futures::{
    channel::oneshot::{channel, Receiver, Sender},
    task::{waker as make_waker, ArcWake, AtomicWaker},
    Future,
};
use js_sys::{
    eval, Function, Number, Object, Reflect, Uint8Array,
    WebAssembly::{compile, instantiate_buffer, Instance, Memory, Module},
};
use std::{
    cell::RefCell,
    pin::Pin,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Poll},
};
use vessels::{
    acquire,
    resource::{ErasedResourceManager, ResourceManagerExt},
    runtime::{Runtime, RuntimeError, WasmResource},
};
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

pub struct WebRuntime;

struct WebFuture<T: AsyncWrite, U: AsyncRead> {
    instance: Instance,
    waker: Arc<AtomicWaker>,
    reader_wakeup: Arc<AtomicBool>,
    writer_write_wakeup: Arc<AtomicBool>,
    writer_flush_wakeup: Arc<AtomicBool>,
    writer_close_wakeup: Arc<AtomicBool>,
    error: Receiver<RuntimeError<JsValue, U::Error, T::WriteError, T::FlushError, T::CloseError>>,
    vessel_poll: Function,
    vessel_wake_reader: Function,
    vessel_wake_writer_write: Function,
    vessel_wake_writer_flush: Function,
    vessel_wake_writer_close: Function,
    vessel_wake: Closure<dyn FnMut()>,
    vessel_poll_read: Closure<dyn FnMut(i32, i32) -> i32>,
    vessel_poll_write: Closure<dyn FnMut(i32, i32) -> i32>,
    vessel_poll_flush: Closure<dyn FnMut() -> i32>,
    vessel_poll_close: Closure<dyn FnMut() -> i32>,
}

impl<T: AsyncWrite, U: AsyncRead> Future for WebFuture<T, U> {
    type Output =
        Result<(), RuntimeError<JsValue, U::Error, T::WriteError, T::FlushError, T::CloseError>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        self.waker.register(cx.waker());

        match self.error.try_recv() {
            Ok(Some(e)) => return Poll::Ready(Err(e)),
            _ => {}
        }

        if self
            .reader_wakeup
            .compare_and_swap(true, false, Ordering::SeqCst)
        {
            self.vessel_wake_reader
                .call0(&self.vessel_poll)
                .map_err(RuntimeError::Runtime)?;
        }

        if self
            .writer_write_wakeup
            .compare_and_swap(true, false, Ordering::SeqCst)
        {
            self.vessel_wake_writer_write
                .call0(&self.vessel_poll)
                .map_err(RuntimeError::Runtime)?;
        }

        if self
            .writer_flush_wakeup
            .compare_and_swap(true, false, Ordering::SeqCst)
        {
            self.vessel_wake_writer_flush
                .call0(&self.vessel_poll)
                .map_err(RuntimeError::Runtime)?;
        }

        if self
            .writer_close_wakeup
            .compare_and_swap(true, false, Ordering::SeqCst)
        {
            self.vessel_wake_writer_close
                .call0(&self.vessel_poll)
                .map_err(RuntimeError::Runtime)?;
        }

        if self
            .vessel_poll
            .call0(&self.vessel_poll)
            .map_err(RuntimeError::Runtime)?
            .dyn_into::<Number>()
            .map_err(RuntimeError::Runtime)?
            .value_of() as i32
            != 0
        {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }
}

struct VesselWaker {
    wakeup: Arc<AtomicBool>,
    waker: Arc<AtomicWaker>,
}

impl ArcWake for VesselWaker {
    fn wake_by_ref(arc: &Arc<Self>) {
        arc.wakeup.store(true, Ordering::SeqCst);
        arc.waker.wake();
    }
}

struct VesselContext<T: AsyncWrite, U: AsyncRead> {
    reader: U,
    writer: T,
    waker: Arc<AtomicWaker>,
    error_sender: Option<
        Sender<RuntimeError<JsValue, U::Error, T::WriteError, T::FlushError, T::CloseError>>,
    >,
    memory: Option<Memory>,
}

impl<T: 'static + Unpin + AsyncWrite, U: 'static + Unpin + AsyncRead> Runtime<T, U> for WebRuntime {
    type Instance = Pin<
        Box<
            dyn Future<
                Output = Result<
                    (),
                    RuntimeError<
                        Self::Error,
                        U::Error,
                        T::WriteError,
                        T::FlushError,
                        T::CloseError,
                    >,
                >,
            >,
        >,
    >;
    type Error = JsValue;

    fn instantiate(&mut self, module: WasmResource, writer: T, reader: U) -> Self::Instance {
        Box::pin(async move {
            let (error_sender, error) = channel();

            let error_sender = Some(error_sender);

            let reader_wakeup = Arc::new(AtomicBool::new(false));
            let writer_write_wakeup = Arc::new(AtomicBool::new(false));
            let writer_flush_wakeup = Arc::new(AtomicBool::new(false));
            let writer_close_wakeup = Arc::new(AtomicBool::new(false));

            let manager: ErasedResourceManager =
                acquire().await?.ok_or(RuntimeError::NoResourceManager)?;

            let fetch = manager.fetch(module);
            let data = fetch.await?.ok_or(RuntimeError::NoBinary)?.0;

            let waker = Arc::new(AtomicWaker::new());

            let waker_handle = waker.clone();

            let reader_waker = make_waker(Arc::new(VesselWaker {
                wakeup: reader_wakeup.clone(),
                waker: waker.clone(),
            }));

            let writer_write_waker = make_waker(Arc::new(VesselWaker {
                wakeup: writer_write_wakeup.clone(),
                waker: waker.clone(),
            }));

            let writer_flush_waker = make_waker(Arc::new(VesselWaker {
                wakeup: writer_write_wakeup.clone(),
                waker: waker.clone(),
            }));

            let writer_close_waker = make_waker(Arc::new(VesselWaker {
                wakeup: writer_write_wakeup.clone(),
                waker: waker.clone(),
            }));

            let context = Rc::new(RefCell::new(VesselContext {
                reader,
                writer,
                waker: waker.clone(),
                memory: None,
                error_sender,
            }));

            let reader_context_handle = context.clone();
            let writer_write_context_handle = context.clone();
            let writer_flush_context_handle = context.clone();
            let writer_close_context_handle = context.clone();

            let import_object = Object::new();
            let imports = Object::new();
            Reflect::set(&import_object, &"module".into(), &Object::new())
                .map_err(RuntimeError::Runtime)?;

            Reflect::set(&import_object, &"env".into(), &imports).map_err(RuntimeError::Runtime)?;

            let vessel_wake = Closure::wrap(Box::new(move || {
                waker_handle.wake();
            }) as Box<dyn FnMut()>);

            Reflect::set(
                &imports,
                &"_vessel_wake".into(),
                vessel_wake.as_ref().unchecked_ref(),
            )
            .map_err(RuntimeError::Runtime)?;

            let vessel_poll_read = Closure::wrap(Box::new(move |ptr: i32, len: i32| -> i32 {
                let context = &mut *(reader_context_handle.borrow_mut());

                let mut buffer = vec![0u8; len as usize];

                match Pin::new(&mut context.reader)
                    .poll_read(&mut Context::from_waker(&reader_waker), &mut buffer)
                {
                    Poll::Pending => 0,
                    Poll::Ready(Ok(len)) => {
                        let view = Uint8Array::new(&context.memory.as_ref().unwrap().buffer());

                        view.set(&Uint8Array::from(&buffer[..len]).into(), ptr as u32);

                        (len + 1) as i32
                    }
                    Poll::Ready(Err(e)) => {
                        if let Some(sender) = context.error_sender.take() {
                            let _ = sender.send(RuntimeError::Read(e));
                        }
                        context.waker.wake();

                        0
                    }
                }
            }) as Box<dyn FnMut(i32, i32) -> i32>);

            Reflect::set(
                &imports,
                &"_vessel_poll_read".into(),
                vessel_poll_read.as_ref().unchecked_ref(),
            )
            .map_err(RuntimeError::Runtime)?;

            let vessel_poll_write = Closure::wrap(Box::new(move |ptr: i32, len: i32| -> i32 {
                let (len, ptr) = (len as usize, ptr as usize);

                let context = &mut *(writer_write_context_handle.borrow_mut());

                let view = Uint8Array::new(&context.memory.as_ref().unwrap().buffer());

                let buffer = view.subarray(ptr as u32, (ptr + len) as u32).to_vec();

                match Pin::new(&mut context.writer)
                    .poll_write(&mut Context::from_waker(&writer_write_waker), &buffer)
                {
                    Poll::Pending => 0,
                    Poll::Ready(Ok(len)) => (len + 1) as i32,
                    Poll::Ready(Err(e)) => {
                        if let Some(sender) = context.error_sender.take() {
                            let _ = sender.send(RuntimeError::Write(e));
                        }
                        context.waker.wake();

                        0
                    }
                }
            })
                as Box<dyn FnMut(i32, i32) -> i32>);

            Reflect::set(
                &imports,
                &"_vessel_poll_write".into(),
                vessel_poll_write.as_ref().unchecked_ref(),
            )
            .map_err(RuntimeError::Runtime)?;

            let vessel_poll_flush = Closure::wrap(Box::new(move || -> i32 {
                let context = &mut *(writer_flush_context_handle.borrow_mut());

                match Pin::new(&mut context.writer)
                    .poll_flush(&mut Context::from_waker(&writer_flush_waker))
                {
                    Poll::Pending => 0,
                    Poll::Ready(Ok(())) => 1,
                    Poll::Ready(Err(e)) => {
                        if let Some(sender) = context.error_sender.take() {
                            let _ = sender.send(RuntimeError::Flush(e));
                        }
                        context.waker.wake();

                        0
                    }
                }
            }) as Box<dyn FnMut() -> i32>);

            Reflect::set(
                &imports,
                &"_vessel_poll_flush".into(),
                vessel_poll_flush.as_ref().unchecked_ref(),
            )
            .map_err(RuntimeError::Runtime)?;

            let vessel_poll_close = Closure::wrap(Box::new(move || -> i32 {
                let context = &mut *(writer_close_context_handle.borrow_mut());

                match Pin::new(&mut context.writer)
                    .poll_close(&mut Context::from_waker(&writer_close_waker))
                {
                    Poll::Pending => 0,
                    Poll::Ready(Ok(())) => 1,
                    Poll::Ready(Err(e)) => {
                        if let Some(sender) = context.error_sender.take() {
                            let _ = sender.send(RuntimeError::Close(e));
                        }
                        context.waker.wake();

                        0
                    }
                }
            }) as Box<dyn FnMut() -> i32>);

            Reflect::set(
                &imports,
                &"_vessel_poll_close".into(),
                vessel_poll_close.as_ref().unchecked_ref(),
            )
            .map_err(RuntimeError::Runtime)?;

            let instantiated = JsFuture::from(instantiate_buffer(&data, &import_object))
                .await
                .map_err(RuntimeError::Runtime)?;
            let instance: Instance = Reflect::get(&instantiated, &"instance".into())
                .map_err(RuntimeError::Runtime)?
                .dyn_into()
                .unwrap();

            let exports = instance.exports();

            context.borrow_mut().memory = Some(
                Reflect::get(&exports, &"memory".into())
                    .map_err(RuntimeError::Runtime)?
                    .dyn_into()
                    .map_err(RuntimeError::Runtime)?,
            );

            let entrypoint = Reflect::get(&exports, &"_vessel_entry".into())
                .map_err(RuntimeError::Runtime)?
                .dyn_into::<Function>()
                .map_err(RuntimeError::Runtime)?;

            let vessel_poll = Reflect::get(&exports, &"_vessel_poll".into())
                .map_err(RuntimeError::Runtime)?
                .dyn_into::<Function>()
                .map_err(RuntimeError::Runtime)?;

            let vessel_wake_reader = Reflect::get(&exports, &"_vessel_wake_reader".into())
                .map_err(RuntimeError::Runtime)?
                .dyn_into::<Function>()
                .map_err(RuntimeError::Runtime)?;

            let vessel_wake_writer_write =
                Reflect::get(&exports, &"_vessel_wake_writer_write".into())
                    .map_err(RuntimeError::Runtime)?
                    .dyn_into::<Function>()
                    .map_err(RuntimeError::Runtime)?;

            let vessel_wake_writer_flush =
                Reflect::get(&exports, &"_vessel_wake_writer_flush".into())
                    .map_err(RuntimeError::Runtime)?
                    .dyn_into::<Function>()
                    .map_err(RuntimeError::Runtime)?;

            let vessel_wake_writer_close =
                Reflect::get(&exports, &"_vessel_wake_writer_close".into())
                    .map_err(RuntimeError::Runtime)?
                    .dyn_into::<Function>()
                    .map_err(RuntimeError::Runtime)?;

            entrypoint
                .call0(&entrypoint)
                .map_err(RuntimeError::Runtime)?;

            WebFuture::<T, U> {
                instance,
                waker,
                reader_wakeup,
                writer_write_wakeup,
                writer_flush_wakeup,
                writer_close_wakeup,
                error,
                vessel_poll,
                vessel_wake_reader,
                vessel_wake_writer_write,
                vessel_wake_writer_flush,
                vessel_wake_writer_close,
                vessel_wake,
                vessel_poll_read,
                vessel_poll_write,
                vessel_poll_flush,
                vessel_poll_close,
            }
            .await
        })
    }
}
