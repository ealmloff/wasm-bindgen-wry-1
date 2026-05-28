use std::cell::Cell;
use std::rc::Rc;
use wasm_bindgen::{Closure, JsValue, wasm_bindgen};

pub(crate) fn test_nested_js_request_keeps_rust_deferred_heap_ref_frame() {
    #[wasm_bindgen(inline_js = r#"
        export function return_option_heap_ref_after_nested_callback(cb) {
            for (let i = 0; i < 64; i++) {
                cb();
            }
            return { label: 77 };
        }

        export function read_deferred_heap_ref_label(obj) {
            return obj.label;
        }
    "#)]
    extern "C" {
        fn return_option_heap_ref_after_nested_callback(
            cb: &Closure<dyn FnMut()>,
        ) -> Option<JsValue>;
        fn read_deferred_heap_ref_label(obj: &JsValue) -> u32;
    }

    let call_count = Rc::new(Cell::new(0));
    let call_count_clone = call_count.clone();
    let callback = Closure::new(move || {
        call_count_clone.set(call_count_clone.get() + 1);
    });

    let value = return_option_heap_ref_after_nested_callback(&callback)
        .expect("JS should return an object inside Some");

    assert_eq!(
        call_count.get(),
        64,
        "nested JS-to-Rust callbacks were not all called"
    );
    assert_eq!(read_deferred_heap_ref_label(&value), 77);
}
