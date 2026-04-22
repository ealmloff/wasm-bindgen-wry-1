use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use futures_util::FutureExt;
use libtest_mimic::{Arguments, Conclusion, Failed, Trial};
use tokio::sync::{mpsc, oneshot};
use wasm_bindgen::{batch::batch_async, wasm_bindgen};

mod add_number_js;
#[allow(clippy::redundant_closure)]
mod async_bindings;
mod borrow_stack;
mod callbacks;
mod catch_attribute;
mod clamped;
mod indexing;
mod is_type_of;
mod jsvalue;
mod module_import;
mod reentrant_callbacks;
mod roundtrip;
mod string_enum;
mod structs;
mod thread_local;

#[wasm_bindgen(inline_js = "export function heap_objects_alive(f) {
    return window.jsHeap.heapObjectsAlive();
}")]
extern "C" {
    /// Get the number of alive JS heap objects
    #[wasm_bindgen(js_name = heap_objects_alive)]
    pub fn heap_objects_alive() -> u32;
}

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum BatchMode {
    NonBatched,
    Batched,
}

impl BatchMode {
    fn suffix(self) -> &'static str {
        match self {
            BatchMode::NonBatched => "nonbatched",
            BatchMode::Batched => "batched",
        }
    }
}

// The futures returned by test bodies (e.g. anything awaiting `JsFuture`) are
// `!Send` because they hold `Rc<RefCell<…>>`. That's fine — they're polled on
// the single-threaded runtime where they were constructed. Only the boxed
// closure that constructs them has to be `Send` to cross the channel.
type TestFuture = Pin<Box<dyn Future<Output = ()>>>;
type TestBody = Box<dyn FnOnce() -> TestFuture + Send>;

struct TestRequest {
    body: TestBody,
    done: oneshot::Sender<Result<(), Failed>>,
}

static TASK_TX: OnceLock<mpsc::UnboundedSender<TestRequest>> = OnceLock::new();

/// Send a test future from a libtest_mimic worker thread back to the runtime
/// thread's driver loop, then block until the driver replies.
fn submit_test(body: TestBody) -> Result<(), Failed> {
    let (done_tx, done_rx) = oneshot::channel();
    TASK_TX
        .get()
        .ok_or_else(|| Failed::from("test driver not initialized"))?
        .send(TestRequest {
            body,
            done: done_tx,
        })
        .map_err(|_| Failed::from("test driver disappeared"))?;
    futures_executor::block_on(done_rx)
        .map_err(|_| Failed::from("test driver dropped reply channel"))?
}

async fn run_with_timeout(fut: impl Future<Output = ()>, mode: BatchMode) {
    let body = async move {
        match mode {
            BatchMode::NonBatched => fut.await,
            BatchMode::Batched => batch_async(fut).await,
        }
    };
    tokio::select! {
        _ = body => {}
        _ = tokio::time::sleep(TEST_TIMEOUT) => {
            panic!("Test timed out after {} seconds", TEST_TIMEOUT.as_secs())
        }
    }
}

fn sync_trial<F>(name: String, mode: BatchMode, f: F) -> Trial
where
    F: Fn() + Send + Sync + Copy + 'static,
{
    Trial::test(name, move || {
        submit_test(Box::new(move || {
            Box::pin(run_with_timeout(async move { f() }, mode))
        }))
    })
}

fn async_trial<Fut, F>(name: String, mode: BatchMode, f: F) -> Trial
where
    F: Fn() -> Fut + Send + Sync + Copy + 'static,
    Fut: Future<Output = ()> + 'static,
{
    Trial::test(name, move || {
        submit_test(Box::new(move || Box::pin(run_with_timeout(f(), mode))))
    })
}

fn trial_name(module: &str, name: &str, mode: BatchMode) -> String {
    format!("{module}::{name}::{}", mode.suffix())
}

/// Generate one trial per (test, batch_mode) pair for sync `fn()` tests.
macro_rules! sync_trials {
    ($trials:expr; $($module:ident :: $name:ident),* $(,)?) => {{
        $(
            for mode in [BatchMode::NonBatched, BatchMode::Batched] {
                $trials.push(sync_trial(
                    trial_name(stringify!($module), stringify!($name), mode),
                    mode,
                    $module::$name,
                ));
            }
        )*
    }};
}

/// Generate one trial per (test, batch_mode) pair for `async fn()` tests.
macro_rules! async_trials {
    ($trials:expr; $($module:ident :: $name:ident),* $(,)?) => {{
        $(
            for mode in [BatchMode::NonBatched, BatchMode::Batched] {
                $trials.push(async_trial(
                    trial_name(stringify!($module), stringify!($name), mode),
                    mode,
                    $module::$name,
                ));
            }
        )*
    }};
}

fn build_trials() -> Vec<Trial> {
    let mut trials: Vec<Trial> = Vec::new();

    sync_trials!(trials;
        add_number_js::test_add_number_js,
        add_number_js::test_add_number_js_batch,
        roundtrip::test_roundtrip,
        callbacks::test_call_callback,
        callbacks::test_mut_dyn_fn,
        callbacks::test_mut_dyn_fnmut,
        callbacks::test_mut_dyn_fn_many_arity,
        callbacks::test_mut_dyn_fnmut_many_arity,
        reentrant_callbacks::test_reentrant_fn_closure,
        reentrant_callbacks::test_interleaved_fn_closures,
        jsvalue::test_jsvalue_constants,
        jsvalue::test_jsvalue_bool,
        jsvalue::test_jsvalue_default,
        jsvalue::test_jsvalue_clone_reserved,
        jsvalue::test_jsvalue_equality,
        jsvalue::test_jsvalue_from_js,
        jsvalue::test_jsvalue_pass_to_js,
        jsvalue::test_jsvalue_as_string,
        jsvalue::test_jsvalue_as_f64,
        jsvalue::test_jsvalue_arithmetic,
        jsvalue::test_jsvalue_bitwise,
        jsvalue::test_jsvalue_comparisons,
        jsvalue::test_jsvalue_loose_eq_coercion,
        jsvalue::test_jsvalue_js_in,
        jsvalue::test_instanceof_basic,
        jsvalue::test_instanceof_is_instance_of,
        jsvalue::test_instanceof_dyn_into,
        jsvalue::test_instanceof_dyn_ref,
        jsvalue::test_partial_eq_bool,
        jsvalue::test_partial_eq_numbers,
        jsvalue::test_partial_eq_strings,
        jsvalue::test_try_from_f64,
        jsvalue::test_try_from_string,
        jsvalue::test_owned_arithmetic_operators,
        jsvalue::test_owned_bitwise_operators,
        jsvalue::test_jscast_as_ref,
        jsvalue::test_as_ref_jsvalue,
        string_enum::test_string_enum_from_str,
        string_enum::test_string_enum_to_str,
        string_enum::test_string_enum_to_jsvalue,
        string_enum::test_string_enum_from_jsvalue,
        string_enum::test_string_enum_pass_to_js,
        string_enum::test_string_enum_receive_from_js,
        catch_attribute::test_catch_throws_error,
        catch_attribute::test_catch_successful_call,
        catch_attribute::test_catch_with_arguments,
        catch_attribute::test_catch_method,
        structs::test_struct_bindings,
        clamped::test_clamped_is_uint8clampedarray,
        clamped::test_clamped_vec_is_uint8clampedarray,
        clamped::test_clamped_js_clamping_behavior,
        clamped::test_clamped_preserves_data,
        clamped::test_clamped_empty,
        clamped::test_clamped_mut_slice,
        borrow_stack::test_borrowed_ref_in_callback,
        borrow_stack::test_borrowed_ref_in_callback_with_return,
        borrow_stack::test_borrowed_ref_nested_frames,
        borrow_stack::test_borrowed_ref_deep_nesting,
        thread_local::test_thread_local,
        thread_local::test_thread_local_window,
        module_import::test_module_import,
        indexing::test_indexing_getter_array,
        indexing::test_indexing_setter_array,
        indexing::test_indexing_deleter_array,
        is_type_of::test_is_type_of_string,
        is_type_of::test_is_type_of_number,
        is_type_of::test_is_type_of_with_dyn_into,
        is_type_of::test_is_type_of_with_dyn_ref,
        is_type_of::test_has_type_with_is_type_of,
    );

    async_trials!(trials;
        callbacks::test_call_callback_async,
        callbacks::test_join_many_callbacks_async,
        async_bindings::test_call_async,
        async_bindings::test_call_async_returning_js_value,
        async_bindings::test_catch_async_call_ok,
        async_bindings::test_catch_async_call_err,
        async_bindings::test_async_method,
        async_bindings::test_async_method_with_catch,
        async_bindings::test_async_static_method,
        async_bindings::test_join_many_async,
    );

    trials
}

fn extract_panic_message(payload: Box<dyn Any + Send>) -> Failed {
    let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "test panicked".to_string()
    };
    Failed::from(msg)
}

fn main() {
    let mut args = Arguments::from_args();
    // The webview/JS context is shared across every test, so trials must run
    // serially. Setting test_threads=1 makes libtest_mimic run them on its
    // calling thread (the spawn_blocking task below).
    args.test_threads = Some(1);

    let trials = build_trials();

    // The libtest_mimic Conclusion travels back from the runtime thread to
    // main() so we can invoke `.exit()` after `run_headless` returns.
    let conclusion: std::sync::Arc<Mutex<Option<Conclusion>>> = Default::default();
    let conclusion_for_app = conclusion.clone();

    wry_launch::run_headless(move || async move {
        let (tx, mut rx) = mpsc::unbounded_channel::<TestRequest>();
        TASK_TX
            .set(tx)
            .map_err(|_| ())
            .expect("test driver already initialized");

        // libtest_mimic blocks its calling thread; run it on a dedicated
        // blocking task so the runtime stays free to drive test futures and
        // service IPC from the webview event loop.
        let runner =
            tokio::task::spawn_blocking(move || libtest_mimic::run(&args, trials));
        tokio::pin!(runner);

        let conc = loop {
            tokio::select! {
                Some(req) = rx.recv() => {
                    let result = AssertUnwindSafe((req.body)())
                        .catch_unwind()
                        .await;
                    let reply = match result {
                        Ok(()) => Ok(()),
                        Err(payload) => Err(extract_panic_message(payload)),
                    };
                    let _ = req.done.send(reply);
                }
                res = &mut runner => {
                    break res.expect("libtest_mimic runner panicked");
                }
            }
        };

        *conclusion_for_app.lock().unwrap() = Some(conc);
    })
    .unwrap();

    if let Some(conc) = conclusion.lock().unwrap().take() {
        conc.exit();
    }
}
