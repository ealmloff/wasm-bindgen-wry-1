use wasm_bindgen::prelude::*;
use wasm_bindgen::wasm_bindgen;

/// Minimal reproduction of IPC buffer exhaustion from
/// https://github.com/DioxusLabs/wasm-bindgen-wry/issues/21
///
/// The original crash:
///   panicked at batch.rs:415: Failed to decode return value: U8BufferEmpty
///   data.is_empty()=true, fn_id=1297, is_batching=false, needs_flush=false
///
/// Root cause: when Closure callbacks are triggered by browser event dispatch
/// (setTimeout, scroll, resize, etc.), the timing between evaluate_script and
/// callback XHRs in the protocol handler causes response data to go missing.
/// Synchronous JS callbacks (for loops) do NOT trigger this.
///
/// This test is tuned to reproduce the race on every run by:
///   * scheduling callbacks in rolling waves (not one big burst), so the
///     evaluate_script / XHR arrival race repeats throughout the test,
///   * making each callback perform many JS->Rust IPC roundtrips with
///     mixed return types (HeapRef, Result, void) — a single misaligned
///     Respond corrupts decoding instead of silently succeeding,
///   * bursting Rust->JS evaluate_script calls on every yield so there are
///     always new Rust Evaluates racing JS's Respond XHRs,
///   * yielding via setTimeout(0) so the event loop alternates callback
///     delivery with Rust-side work on the tightest possible cadence.
pub(crate) async fn test_batch_stress_browser_event_callbacks() {
    use wasm_bindgen::Closure;

    #[wasm_bindgen(inline_js = r#"
        export function schedule_callbacks(cb, count) {
            for (let i = 0; i < count; i++) {
                setTimeout(() => cb(i), 0);
            }
        }
        export function yield_to_event_loop() {
            return new Promise(resolve => setTimeout(resolve, 0));
        }
    "#)]
    extern "C" {
        fn schedule_callbacks(cb: &Closure<dyn FnMut(u32)>, count: u32);

        #[wasm_bindgen(catch)]
        async fn yield_to_event_loop() -> Result<JsValue, JsValue>;
    }

    // Each of these knobs is turned up to keep as many Rust Evaluates in
    // flight against as many callback XHRs as possible.
    //
    // WAVE_COUNT * WAVE_SIZE = total callbacks fired; waves are scheduled
    // between Rust-side bursts so the race window recurs throughout the run.
    const WAVE_COUNT: u32 = 8;
    const WAVE_SIZE: u32 = 250;
    const TOTAL_CALLBACKS: u32 = WAVE_COUNT * WAVE_SIZE;
    const RUST_OPS_PER_YIELD: u32 = 40;
    const STALL_LIMIT: u32 = 300;
    const MAX_ITERATIONS: u32 = 20_000;

    let window = web_sys::window().unwrap();
    let document = window.document().unwrap();
    let body = document.body().unwrap();

    let test_start = std::time::Instant::now();
    println!("[batch_stress] t=0ms start");

    let container = document.create_element("div").unwrap();
    body.append_child(&container).unwrap();

    let counter = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let counter_clone = counter.clone();
    let document_clone = document.clone();
    let container_clone = container.clone();

    // Each callback does ~12 JS->Rust roundtrips covering the three return
    // shapes that exercise decode: HeapRef (create_element, append_child),
    // Result<(), JsValue> (set_attribute), and void (set_text_content).
    // If a Respond gets misaligned, these mismatched decoders surface it
    // immediately as U8BufferEmpty rather than corrupting state silently.
    let callback = Closure::new(move |i: u32| {
        counter_clone.set(counter_clone.get() + 1);

        let item = document_clone.create_element("div").unwrap();
        item.set_attribute("class", "grid-item").unwrap();
        item.set_attribute(
            "style",
            &format!(
                "position:absolute;top:{}px;left:{}px;width:200px;height:280px",
                (i / 5) * 296,
                (i % 5) * 216,
            ),
        )
        .unwrap();

        let cover = document_clone.create_element("div").unwrap();
        cover
            .set_attribute("style", "width:200px;height:200px;background:#333")
            .unwrap();
        cover.set_attribute("data-cover", &format!("{i}")).unwrap();
        item.append_child(&cover).unwrap();

        let text = document_clone.create_element("div").unwrap();
        text.set_text_content(Some(&format!("Album {i}")));
        text.set_attribute("data-text", &format!("{i}")).unwrap();
        item.append_child(&text).unwrap();

        let caption = document_clone.create_element("span").unwrap();
        caption.set_text_content(Some(&format!("#{i}")));
        item.append_child(&caption).unwrap();

        container_clone.append_child(&item).unwrap();
    });

    let mut iterations = 0u32;
    let mut last_count = 0u32;
    let mut stalled = 0u32;
    let mut waves_scheduled = 0u32;

    // Schedule the first wave to prime the JS task queue.
    schedule_callbacks(&callback, WAVE_SIZE);
    waves_scheduled += 1;
    println!(
        "[batch_stress] t={}ms scheduled wave 1 ({WAVE_SIZE} callbacks)",
        test_start.elapsed().as_millis()
    );

    while counter.get() < TOTAL_CALLBACKS {
        // Burst many evaluate_script calls per yield to amplify the race
        // with in-flight callback XHRs.
        for _ in 0..RUST_OPS_PER_YIELD {
            let div = document.create_element("div").unwrap();
            div.set_text_content(Some("Rust-side element"));
            body.append_child(&div).unwrap();
        }

        // Keep feeding setTimeout callbacks throughout the test. Scheduling
        // mid-run (rather than all upfront) means new callbacks keep racing
        // Rust's evaluate_scripts for the whole duration.
        if waves_scheduled < WAVE_COUNT
            && counter.get() >= waves_scheduled * WAVE_SIZE / 2
        {
            schedule_callbacks(&callback, WAVE_SIZE);
            waves_scheduled += 1;
        }

        yield_to_event_loop().await.unwrap();
        iterations += 1;

        let now = counter.get();
        if iterations == 1 || iterations % 50 == 0 {
            println!(
                "[batch_stress] t={}ms iter={iterations} counter={now}/{TOTAL_CALLBACKS} waves={waves_scheduled}/{WAVE_COUNT}",
                test_start.elapsed().as_millis()
            );
        }
        if now == last_count {
            stalled += 1;
        } else {
            stalled = 0;
            last_count = now;
        }

        assert!(
            stalled <= STALL_LIMIT,
            "Callbacks stalled at {now}/{TOTAL_CALLBACKS} after {iterations} \
             iterations ({stalled} stalled yields) — likely dropped or \
             mis-decoded callback response",
        );

        assert!(
            iterations < MAX_ITERATIONS,
            "Hit MAX_ITERATIONS={MAX_ITERATIONS} with only {now}/{TOTAL_CALLBACKS} callbacks",
        );
    }

    assert_eq!(
        counter.get(),
        TOTAL_CALLBACKS,
        "Expected exactly {TOTAL_CALLBACKS} callbacks, got {}",
        counter.get()
    );

    callback.forget();
}
