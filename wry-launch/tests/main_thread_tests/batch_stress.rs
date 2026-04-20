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
/// The test maximizes the race by:
///   * queuing a large burst of setTimeout(0) callbacks that each perform
///     multiple JS->Rust IPC roundtrips (DOM calls),
///   * simultaneously bursting Rust->JS evaluate_script calls on every yield,
///   * yielding via setTimeout(0) (not a millisecond sleep) so the event loop
///     interleaves callback delivery with Rust-side work as tightly as possible,
///   * bailing out early with a clear diagnostic if progress stalls.
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

    // Tuned to reproduce the U8BufferEmpty decode failure within a few
    // hundred milliseconds on the affected builds. If you find the bug no
    // longer reproduces reliably, bump CALLBACK_COUNT first.
    const CALLBACK_COUNT: u32 = 500;
    const RUST_OPS_PER_YIELD: u32 = 20;
    const STALL_LIMIT: u32 = 200;
    const MAX_ITERATIONS: u32 = 5_000;

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
        item.append_child(&cover).unwrap();

        let text = document_clone.create_element("div").unwrap();
        text.set_text_content(Some(&format!("Album {i}")));
        item.append_child(&text).unwrap();

        container_clone.append_child(&item).unwrap();
    });

    schedule_callbacks(&callback, CALLBACK_COUNT);
    println!(
        "[batch_stress] t={}ms scheduled {CALLBACK_COUNT} callbacks",
        test_start.elapsed().as_millis()
    );

    let mut iterations = 0u32;
    let mut last_count = 0u32;
    let mut stalled = 0u32;

    while counter.get() < CALLBACK_COUNT {
        // Burst many evaluate_script calls per yield to amplify the race with
        // in-flight callback XHRs.
        for _ in 0..RUST_OPS_PER_YIELD {
            let div = document.create_element("div").unwrap();
            div.set_text_content(Some("Rust-side element"));
            body.append_child(&div).unwrap();
        }

        yield_to_event_loop().await.unwrap();
        iterations += 1;

        let now = counter.get();
        if iterations == 1 || iterations % 20 == 0 {
            println!(
                "[batch_stress] t={}ms iter={iterations} counter={now}",
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
            "Callbacks stalled at {now}/{CALLBACK_COUNT} after {iterations} \
             iterations ({stalled} stalled yields) — likely dropped or \
             mis-decoded callback response",
        );

        assert!(
            iterations < MAX_ITERATIONS,
            "Hit MAX_ITERATIONS={MAX_ITERATIONS} with only {now}/{CALLBACK_COUNT} callbacks",
        );
    }

    assert_eq!(
        counter.get(),
        CALLBACK_COUNT,
        "Expected exactly {CALLBACK_COUNT} callbacks, got {}",
        counter.get()
    );

    callback.forget();
}
