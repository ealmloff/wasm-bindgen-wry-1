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
mod batch_stress;
mod borrow_stack;
mod callbacks;
mod catch_attribute;
mod clamped;
mod export_call;
mod indexing;
mod is_type_of;
mod jsvalue;
mod module_import;
mod opaque_id_stress;
mod reentrant_callbacks;
mod roundtrip;
mod string_enum;
mod structs;
mod thread_local;
mod wasm_bindgen_compat;

#[wasm_bindgen(inline_js = "export function heap_objects_alive(f) {
    return window.jsHeap.heapObjectsAlive();
}")]
extern "C" {
    /// Get the number of alive JS heap objects
    #[wasm_bindgen(js_name = heap_objects_alive)]
    pub fn heap_objects_alive() -> u32;
}

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const STRESS_TEST_TIMEOUT: Duration = Duration::from_secs(30);

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

#[derive(Clone, Copy, Default)]
struct HarnessOptions {
    repeat_for: Option<Duration>,
    repro_exported_struct_heap_ref: bool,
}

// The futures returned by test bodies are !Send because they can hold
// Rc<RefCell<_>> values. They are polled on the single-threaded runtime where
// they were constructed.
type TestFuture = Pin<Box<dyn Future<Output = ()>>>;
type TestBody = Box<dyn FnOnce() -> TestFuture>;

struct TestCase {
    name: String,
    body: TestBody,
}

async fn run_with_timeout(fut: impl Future<Output = ()>, mode: BatchMode, timeout: Duration) {
    let body = async move {
        match mode {
            BatchMode::NonBatched => fut.await,
            BatchMode::Batched => batch_async(fut).await,
        }
    };
    tokio::select! {
        _ = body => {}
        _ = tokio::time::sleep(timeout) => {
            panic!("Test timed out after {} seconds", timeout.as_secs())
        }
    }
}

fn sync_test<F>(name: String, mode: BatchMode, f: F) -> TestCase
where
    F: Fn() + Copy + 'static,
{
    sync_test_with_timeout(name, mode, TEST_TIMEOUT, f)
}

fn sync_test_with_timeout<F>(name: String, mode: BatchMode, timeout: Duration, f: F) -> TestCase
where
    F: Fn() + Copy + 'static,
{
    TestCase {
        name,
        body: Box::new(move || Box::pin(run_with_timeout(async move { f() }, mode, timeout))),
    }
}

fn async_test<Fut, F>(name: String, mode: BatchMode, f: F) -> TestCase
where
    F: Fn() -> Fut + Copy + 'static,
    Fut: Future<Output = ()> + 'static,
{
    async_test_with_timeout(name, mode, TEST_TIMEOUT, f)
}

fn async_test_with_timeout<Fut, F>(
    name: String,
    mode: BatchMode,
    timeout: Duration,
    f: F,
) -> TestCase
where
    F: Fn() -> Fut + Copy + 'static,
    Fut: Future<Output = ()> + 'static,
{
    TestCase {
        name,
        body: Box::new(move || Box::pin(run_with_timeout(f(), mode, timeout))),
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

    tests.push(async_test_with_timeout(
        trial_name(
            "opaque_id_stress",
            "test_opaque_id_double_free_stress",
            BatchMode::Batched,
        ),
        BatchMode::Batched,
        STRESS_TEST_TIMEOUT,
        opaque_id_stress::test_opaque_id_double_free_stress,
    ));
    tests.push(async_test_with_timeout(
        trial_name(
            "batch_stress",
            "test_batch_stress_browser_event_callbacks",
            BatchMode::Batched,
        ),
        BatchMode::Batched,
        STRESS_TEST_TIMEOUT,
        batch_stress::test_batch_stress_browser_event_callbacks,
    ));

    sync_trials!(tests;
        add_number_js::test_add_number_js,
        add_number_js::test_add_number_js_batch,
        roundtrip::test_roundtrip,
        callbacks::test_call_callback,
        callbacks::test_dropped_closure_disposes_js_callable,
        callbacks::test_exported_method_drop_closure_disposes_js_callable,
        callbacks::test_mut_dyn_fn,
        callbacks::test_mut_dyn_fnmut,
        callbacks::test_batch_flushed_heap_ref_return_with_stack_callback,
        callbacks::test_js_callback_heap_ref_arg_with_pending_placeholders,
        callbacks::test_js_callback_multiple_heap_ref_args_share_request_id,
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
        export_call::test_js_calls_exported_usize_js_thunk,
        export_call::test_js_calls_exported_usize_js_thunk_batched,
        clamped::test_clamped_is_uint8clampedarray,
        clamped::test_clamped_vec_is_uint8clampedarray,
        clamped::test_jsvalue_from_clamped_vec_is_uint8clampedarray,
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
        wasm_bindgen_compat::test_imported_type_promising_compat,
        wasm_bindgen_compat::test_convert_traits_are_marker_bounds,
        wasm_bindgen_compat::test_interned_string_roundtrip,
        wasm_bindgen_compat::test_jsvalue_abi_ref_preserves_heap_ref,
        wasm_bindgen_compat::test_u128_try_from_bigint_preserves_range,
        wasm_bindgen_compat::test_i128_try_from_bigint_preserves_full_width,
        wasm_bindgen_compat::test_try_from_js_value_signed_numbers_preserve_negative_values,
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

fn build_repro_tests() -> Vec<TestCase> {
    let mut tests: Vec<TestCase> = Vec::new();

    sync_trials!(tests;
        structs::test_exported_struct_arg_before_heap_ref_arg,
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

fn parse_harness_args() -> (HarnessOptions, Vec<String>) {
    let mut options = HarnessOptions::default();
    let mut libtest_args = Vec::new();

    let mut args = std::env::args();
    if let Some(executable) = args.next() {
        libtest_args.push(executable);
    }

    for arg in args {
        if arg == "--wry-repro-exported-struct-heap-ref" {
            options.repro_exported_struct_heap_ref = true;
        } else if let Some(value) = arg.strip_prefix("--wry-repeat-for-secs=") {
            options.repeat_for = value.parse::<u64>().ok().map(Duration::from_secs);
        } else {
            libtest_args.push(arg);
        }
    }

    if options.repeat_for.is_none() {
        options.repeat_for = std::env::var("WRY_BINDGEN_REPEAT_FOR_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs);
    }

    (options, libtest_args)
}

fn selected_tests(options: HarnessOptions) -> Vec<TestCase> {
    if options.repro_exported_struct_heap_ref {
        build_repro_tests()
    } else {
        build_tests()
    }
}

async fn run_selected_tests(args: Arguments, options: HarnessOptions) -> bool {
    run_tests(args, selected_tests(options)).await
}

fn main() -> ExitCode {
    let (options, libtest_args) = parse_harness_args();
    let args = Arguments::from_iter(libtest_args);

    // The test result travels back from the runtime thread to main() so
    // `main` can return its exit code after `run_headless` returns.
    let (passed_tx, passed_rx) = std_mpsc::channel::<bool>();

    wry_launch::run_headless(move || async move {
        let passed = if let Some(duration) = options.repeat_for.filter(|_| !args.list) {
            let start = Instant::now();
            let mut iteration = 0u64;

            loop {
                iteration += 1;
                println!(
                    "=== main_thread_tests iteration {iteration} elapsed {:?} ===",
                    start.elapsed()
                );

                if !run_selected_tests(args.clone(), options).await {
                    break false;
                }

                if start.elapsed() >= duration {
                    println!(
                        "=== completed {iteration} clean main_thread_tests iterations in {:?} ===",
                        start.elapsed()
                    );
                    break true;
                }
            }
        } else {
            run_selected_tests(args, options).await
        };

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
