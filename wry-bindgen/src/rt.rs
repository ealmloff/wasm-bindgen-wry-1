//! Runtime compatibility hooks used by generated wasm-bindgen-style code.

use crate::{
    __wry_submit_js_function, Closure, JsValue,
    encode::{BatchableResult, BinaryEncode, EncodeTypeDef},
};
use alloc::rc::Rc;
use core::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

#[doc(hidden)]
pub use crate::batch::Runtime;
#[doc(hidden)]
pub use crate::encode::TypeTag;
#[doc(hidden)]
pub use crate::function_registry::{
    InlineJsModule, JsClassMemberKind, JsClassMemberSpec, JsExportSpec, JsFunctionSpec,
    LazyJsFunction,
};
#[doc(hidden)]
pub use crate::js_helpers::js_extract_rust_handle as extract_rust_handle;
#[doc(hidden)]
pub use inventory;

#[doc(hidden)]
pub mod object_store {
    pub use crate::object_store::{
        ObjectHandle, create_js_wrapper, drop_object, insert_object, remove_object, with_object,
        with_object_mut,
    };
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WasmWord(pub u32);

pub struct Ref<'b, T: ?Sized + 'b> {
    pub(crate) inner: core::cell::Ref<'b, T>,
}

impl<T: ?Sized> core::ops::Deref for Ref<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: ?Sized> core::borrow::Borrow<T> for Ref<'_, T> {
    fn borrow(&self) -> &T {
        self
    }
}

pub struct RefMut<'b, T: ?Sized + 'b> {
    pub(crate) inner: core::cell::RefMut<'b, T>,
}

impl<T: ?Sized> core::ops::Deref for RefMut<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: ?Sized> core::ops::DerefMut for RefMut<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<T: ?Sized> core::borrow::Borrow<T> for RefMut<'_, T> {
    fn borrow(&self) -> &T {
        self
    }
}

impl<T: ?Sized> core::borrow::BorrowMut<T> for RefMut<'_, T> {
    fn borrow_mut(&mut self) -> &mut T {
        self
    }
}

pub mod marker {
    /// Marker for types whose generic parameters erase to one stable runtime representation.
    ///
    /// # Safety
    /// Implementors must have the same runtime representation as `Repr`.
    pub unsafe trait ErasableGeneric {
        type Repr: 'static;
    }

    unsafe impl<T: ErasableGeneric> ErasableGeneric for &T {
        type Repr = &'static T::Repr;
    }

    unsafe impl<T: ErasableGeneric> ErasableGeneric for &mut T {
        type Repr = &'static mut T::Repr;
    }

    /// Marker for owned generic values whose representation matches a concrete target.
    pub trait ErasableGenericOwn<ConcreteTarget>: ErasableGeneric {}

    impl<T, ConcreteTarget> ErasableGenericOwn<ConcreteTarget> for T
    where
        ConcreteTarget: ErasableGeneric,
        T: ErasableGeneric<Repr = <ConcreteTarget as ErasableGeneric>::Repr>,
    {
    }

    /// Marker for borrowed generic values whose representation matches a concrete target.
    pub trait ErasableGenericBorrow<Target: ?Sized> {}

    impl<'a, T: ?Sized + 'a, ConcreteTarget: ?Sized + 'static> ErasableGenericBorrow<ConcreteTarget>
        for T
    where
        &'static ConcreteTarget: ErasableGeneric,
        &'a T: ErasableGeneric<Repr = <&'static ConcreteTarget as ErasableGeneric>::Repr>,
    {
    }

    /// Marker for mutably borrowed generic values whose representation matches a concrete target.
    pub trait ErasableGenericBorrowMut<Target: ?Sized> {}

    impl<'a, T: ?Sized + 'a, ConcreteTarget: ?Sized + 'static>
        ErasableGenericBorrowMut<ConcreteTarget> for T
    where
        &'static mut ConcreteTarget: ErasableGeneric,
        &'a mut T: ErasableGeneric<Repr = <&'static mut ConcreteTarget as ErasableGeneric>::Repr>,
    {
    }
}

/// Cast between types via the binary protocol.
///
/// This is the wry-bindgen equivalent of wasm-bindgen's wbg_cast.
/// It encodes `value` using From's BinaryEncode, sends to JS as identity,
/// and decodes the result using To's BinaryDecode.
#[inline]
pub fn wbg_cast<From, To>(value: From) -> To
where
    From: BinaryEncode + EncodeTypeDef,
    To: BatchableResult + EncodeTypeDef,
{
    let func: LazyJsFunction<fn(From) -> To> = __wry_submit_js_function!("(a0) => a0");
    func.call(value)
}

pub type PromiseCallback = Closure<dyn FnMut(JsValue)>;
type PromiseThenWithReject = fn(&JsValue, &PromiseCallback, &PromiseCallback);

struct PromiseCallbacks {
    _resolve: PromiseCallback,
    _reject: PromiseCallback,
}

enum PromiseFutureState {
    /// `then` has been called, but the Rust-to-JS attach call has not
    /// returned yet. JS may still synchronously call back into Rust here.
    Attaching,
    /// The Promise is unresolved and the callback pair must stay alive.
    Pending(PromiseCallbacks),
    /// The Promise has settled and is waiting to be observed by `poll`.
    Ready(Result<JsValue, JsValue>),
    /// The future has completed or was dropped.
    Consumed,
}

struct PromiseFutureInner {
    state: PromiseFutureState,
    task: Option<Waker>,
}

/// Future adapter for Promise values returned by generated async imports.
///
/// Unlike upstream wasm, wry's Rust-to-JS call that installs `then`
/// callbacks crosses the event loop. A settled Promise can therefore call
/// back into Rust before the attaching Rust stack resumes, so this state
/// machine accepts completion before the callback pair has been stored.
pub struct PromiseFuture {
    inner: Rc<RefCell<PromiseFutureInner>>,
}

fn finish_promise_future(inner: &Rc<RefCell<PromiseFutureInner>>, value: Result<JsValue, JsValue>) {
    let (task, callbacks_to_drop) = {
        let mut inner = inner.borrow_mut();
        let callbacks_to_drop =
            match core::mem::replace(&mut inner.state, PromiseFutureState::Ready(value)) {
                PromiseFutureState::Attaching => None,
                PromiseFutureState::Pending(callbacks) => Some(callbacks),
                PromiseFutureState::Ready(existing) => {
                    inner.state = PromiseFutureState::Ready(existing);
                    return;
                }
                PromiseFutureState::Consumed => {
                    inner.state = PromiseFutureState::Consumed;
                    return;
                }
            };
        (inner.task.take(), callbacks_to_drop)
    };
    drop(callbacks_to_drop);

    if let Some(task) = task {
        task.wake();
    }
}

fn promise_then_with_reject(
    promise: &JsValue,
    resolve: &PromiseCallback,
    reject: &PromiseCallback,
) {
    let func: LazyJsFunction<PromiseThenWithReject> =
        __wry_submit_js_function!("(obj, resolve, reject) => obj[\"then\"](resolve, reject)");
    func.call(promise, resolve, reject);
}

pub fn promise_to_future_with_callbacks(
    attach: impl FnOnce(&PromiseCallback, &PromiseCallback),
) -> PromiseFuture {
    let inner = Rc::new(RefCell::new(PromiseFutureInner {
        state: PromiseFutureState::Attaching,
        task: None,
    }));

    let resolve = {
        let inner = inner.clone();
        Closure::<dyn FnMut(JsValue)>::once(move |value| {
            finish_promise_future(&inner, Ok(value));
        })
    };

    let reject = {
        let inner = inner.clone();
        Closure::<dyn FnMut(JsValue)>::once(move |value| {
            finish_promise_future(&inner, Err(value));
        })
    };

    attach(&resolve, &reject);

    let callbacks = PromiseCallbacks {
        _resolve: resolve,
        _reject: reject,
    };
    let mut inner_mut = inner.borrow_mut();
    let callbacks_to_drop = if matches!(inner_mut.state, PromiseFutureState::Attaching) {
        inner_mut.state = PromiseFutureState::Pending(callbacks);
        None
    } else {
        Some(callbacks)
    };
    drop(inner_mut);
    drop(callbacks_to_drop);

    PromiseFuture { inner }
}

pub fn promise_to_future(promise: JsValue) -> PromiseFuture {
    promise_to_future_with_callbacks(|resolve, reject| {
        promise_then_with_reject(&promise, resolve, reject);
    })
}

impl Future for PromiseFuture {
    type Output = Result<JsValue, JsValue>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut inner = self.inner.borrow_mut();
        match core::mem::replace(&mut inner.state, PromiseFutureState::Consumed) {
            PromiseFutureState::Ready(result) => Poll::Ready(result),
            state @ (PromiseFutureState::Attaching | PromiseFutureState::Pending(_)) => {
                inner.state = state;
                inner.task = Some(cx.waker().clone());
                Poll::Pending
            }
            PromiseFutureState::Consumed => {
                panic!("PromiseFuture polled after completion")
            }
        }
    }
}

impl Drop for PromiseFuture {
    fn drop(&mut self) {
        let callbacks_to_drop = match core::mem::replace(
            &mut self.inner.borrow_mut().state,
            PromiseFutureState::Consumed,
        ) {
            PromiseFutureState::Pending(callbacks) => Some(callbacks),
            _ => None,
        };
        drop(callbacks_to_drop);
    }
}

/// Convert a panic value into a JsValue error.
///
/// This is used by wasm-bindgen-futures to convert Rust panics into JS errors.
#[cfg(feature = "std")]
pub fn panic_to_panic_error(val: std::boxed::Box<dyn std::any::Any + Send>) -> JsValue {
    let maybe_panic_msg: Option<&str> = if let Some(s) = val.downcast_ref::<&str>() {
        Some(s)
    } else if let Some(s) = val.downcast_ref::<std::string::String>() {
        Some(s)
    } else {
        None
    };
    // Create an Error object with the panic message
    JsValue::from_str(maybe_panic_msg.unwrap_or("Rust panic"))
}
