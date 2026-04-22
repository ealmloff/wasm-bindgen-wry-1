use std::any::Any;
use std::future::Future;
use std::io::{self, Write};
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use libtest_mimic::{Arguments, Failed};
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
// the single-threaded runtime where they were constructed.
type TestFuture = Pin<Box<dyn Future<Output = ()>>>;
type TestBody = Box<dyn FnOnce() -> TestFuture>;

struct TestCase {
    name: String,
    body: TestBody,
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

fn sync_test<F>(name: String, mode: BatchMode, f: F) -> TestCase
where
    F: Fn() + Copy + 'static,
{
    TestCase {
        name,
        body: Box::new(move || Box::pin(run_with_timeout(async move { f() }, mode))),
    }
}

fn async_test<Fut, F>(name: String, mode: BatchMode, f: F) -> TestCase
where
    F: Fn() -> Fut + Copy + 'static,
    Fut: Future<Output = ()> + 'static,
{
    TestCase {
        name,
        body: Box::new(move || Box::pin(run_with_timeout(f(), mode))),
    }
}

fn trial_name(module: &str, name: &str, mode: BatchMode) -> String {
    format!("{module}::{name}::{}", mode.suffix())
}

/// Generate one case per (test, batch_mode) pair for sync `fn()` tests.
macro_rules! sync_trials {
    ($tests:expr; $($module:ident :: $name:ident),* $(,)?) => {{
        $(
            for mode in [BatchMode::NonBatched, BatchMode::Batched] {
                $tests.push(sync_test(
                    trial_name(stringify!($module), stringify!($name), mode),
                    mode,
                    $module::$name,
                ));
            }
        )*
    }};
}

/// Generate one case per (test, batch_mode) pair for `async fn()` tests.
macro_rules! async_trials {
    ($tests:expr; $($module:ident :: $name:ident),* $(,)?) => {{
        $(
            for mode in [BatchMode::NonBatched, BatchMode::Batched] {
                $tests.push(async_test(
                    trial_name(stringify!($module), stringify!($name), mode),
                    mode,
                    $module::$name,
                ));
            }
        )*
    }};
}

fn build_tests() -> Vec<TestCase> {
    let mut tests: Vec<TestCase> = Vec::new();

    sync_trials!(tests;
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

    async_trials!(tests;
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

    tests
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

async fn run_test(body: TestBody) -> Result<(), Failed> {
    let fut = std::panic::catch_unwind(AssertUnwindSafe(body)).map_err(extract_panic_message)?;
    AssertUnwindSafe(fut)
        .catch_unwind()
        .await
        .map_err(extract_panic_message)
}

fn is_filtered_out(args: &Arguments, test: &TestCase) -> bool {
    if let Some(filter) = &args.filter {
        match args.exact {
            true if &test.name != filter => return true,
            false if !test.name.contains(filter) => return true,
            _ => {}
        }
    }

    for skip_filter in &args.skip {
        match args.exact {
            true if &test.name == skip_filter => return true,
            false if test.name.contains(skip_filter) => return true,
            _ => {}
        }
    }

    // This harness does not define ignored tests, so `--ignored` filters every
    // test out just like libtest would.
    args.ignored
}

fn print_failures(failures: &[(String, Option<String>)]) {
    if failures.is_empty() {
        return;
    }

    println!();
    println!("failures:");
    println!();

    for (name, message) in failures {
        println!("---- {name} ----");
        if let Some(message) = message {
            println!("{message}");
        }
        println!();
    }

    println!();
    println!("failures:");
    for (name, _) in failures {
        println!("    {name}");
    }
}

async fn run_tests(args: Arguments, mut tests: Vec<TestCase>) -> bool {
    let started = Instant::now();
    let initial_count = tests.len();
    tests.retain(|test| !is_filtered_out(&args, test));
    let filtered = initial_count - tests.len();

    if args.list {
        for test in tests {
            println!("{}: test", test.name);
        }
        return true;
    }

    let plural = if tests.len() == 1 { "" } else { "s" };
    println!();
    println!("running {} test{plural}", tests.len());

    let name_width = tests
        .iter()
        .map(|test| test.name.chars().count())
        .max()
        .unwrap_or(0);
    let mut passed = 0;
    let mut ignored = 0;
    let mut failures = Vec::new();

    for test in tests {
        print!("test {: <name_width$} ... ", test.name);
        io::stdout().flush().unwrap();

        if args.bench {
            ignored += 1;
            println!("ignored");
            continue;
        }

        match run_test(test.body).await {
            Ok(()) => {
                passed += 1;
                println!("ok");
            }
            Err(failed) => {
                println!("FAILED");
                failures.push((test.name.clone(), failed.message().map(ToOwned::to_owned)));
            }
        }
    }

    print_failures(&failures);

    let result = if failures.is_empty() { "ok" } else { "FAILED" };
    println!();
    println!(
        "test result: {result}. {passed} passed; {} failed; {ignored} ignored; 0 measured; {filtered} filtered out; finished in {:.2}s",
        failures.len(),
        started.elapsed().as_secs_f64(),
    );
    println!();

    failures.is_empty()
}

fn main() -> ExitCode {
    let args = Arguments::from_args();

    // The test result travels back from the runtime thread to main() so
    // `main` can return its exit code after `run_headless` returns.
    let (passed_tx, passed_rx) = std_mpsc::channel::<bool>();

    wry_launch::run_headless(move || async move {
        let passed = run_tests(args, build_tests()).await;
        passed_tx
            .send(passed)
            .expect("test result receiver disappeared");
    })
    .expect("failed to run headless test harness");

    if passed_rx.recv().expect("test result sender disappeared") {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(101)
    }
}
