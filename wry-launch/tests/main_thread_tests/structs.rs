use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = "export function increment_by_5(s) {
    for (let i = 0; i < 5; i++)
        s.increment();
}
export function set_count(s, count) {
    s.count = count;
}
export function get_count(s) {
    return s.count;
}")]
extern "C" {
    fn increment_by_5(s: &JsValue);
    fn set_count(s: &JsValue, count: i32);
    fn get_count(s: &JsValue) -> i32;
}

#[wasm_bindgen(
    inline_js = r#"
export function create_text_bridge(label) {
    return TextBridge.new(label);
}

export function get_text_label(bridge) {
    return bridge.label;
}

export function set_text_label(bridge, label) {
    bridge.label = label;
}

export function join_text(bridge, suffix) {
    return bridge.join(suffix);
}

export function swap_text(bridge, next) {
    return bridge.swap(next);
}

export function optional_text(bridge, key) {
    return bridge.optional(key);
}

export function static_compose(prefix, suffix) {
    return TextBridge.compose(prefix, suffix);
}
"#
)]
extern "C" {
    fn create_text_bridge(label: &str) -> JsValue;
    fn get_text_label(bridge: &JsValue) -> String;
    fn set_text_label(bridge: &JsValue, label: &str);
    fn join_text(bridge: &JsValue, suffix: &str) -> String;
    fn swap_text(bridge: &JsValue, next: &str) -> String;
    fn optional_text(bridge: &JsValue, key: &str) -> Option<String>;
    fn static_compose(prefix: &str, suffix: &str) -> String;
}

#[wasm_bindgen]
#[derive(Debug)]
pub struct Counter {
    count: i32,
}

#[wasm_bindgen]
impl Counter {
    #[wasm_bindgen(constructor)]
    pub fn new(count: i32) -> Counter {
        Counter { count }
    }

    #[wasm_bindgen(getter)]
    pub fn count(&self) -> i32 {
        self.count
    }

    #[wasm_bindgen(setter)]
    pub fn set_count(&mut self, count: i32) {
        self.count = count * 2;
    }

    pub fn increment(&mut self) {
        self.count += 1;
    }
}

#[wasm_bindgen]
pub struct TextBridge {
    label: String,
}

#[wasm_bindgen]
impl TextBridge {
    #[wasm_bindgen(constructor)]
    pub fn new(label: String) -> TextBridge {
        TextBridge { label }
    }

    #[wasm_bindgen(getter)]
    pub fn label(&self) -> String {
        self.label.clone()
    }

    #[wasm_bindgen(setter)]
    pub fn set_label(&mut self, label: String) {
        self.label = format!("set:{label}");
    }

    pub fn join(&self, suffix: String) -> String {
        format!("{}::{suffix}", self.label)
    }

    pub fn swap(&mut self, next: String) -> String {
        let previous = self.label.clone();
        self.label = next;
        previous
    }

    pub fn optional(&self, key: String) -> Option<String> {
        match key.as_str() {
            "present" => Some(format!("{}::{key}", self.label)),
            _ => None,
        }
    }

    pub fn compose(prefix: String, suffix: String) -> String {
        format!("{prefix}+{suffix}")
    }
}

pub(crate) fn test_struct_bindings() {
    let counter = Counter::new(0);
    assert_eq!(counter.count(), 0);
    let as_js_value = JsValue::from(counter);
    increment_by_5(&as_js_value);
    assert_eq!(get_count(&as_js_value), 5);
    set_count(&as_js_value, 10);
    assert_eq!(get_count(&as_js_value), 20);
}

pub(crate) fn test_struct_typed_bindings() {
    let bridge = create_text_bridge("alpha");

    assert_eq!(get_text_label(&bridge), "alpha");
    assert_eq!(join_text(&bridge, "beta"), "alpha::beta");

    set_text_label(&bridge, "gamma");
    assert_eq!(get_text_label(&bridge), "set:gamma");

    assert_eq!(swap_text(&bridge, "delta"), "set:gamma");
    assert_eq!(get_text_label(&bridge), "delta");

    assert_eq!(
        optional_text(&bridge, "present"),
        Some("delta::present".to_string())
    );
    assert_eq!(optional_text(&bridge, "missing"), None);

    assert_eq!(static_compose("left", "right"), "left+right");
}
