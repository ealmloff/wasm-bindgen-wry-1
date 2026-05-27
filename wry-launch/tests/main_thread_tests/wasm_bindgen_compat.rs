use wasm_bindgen::{
    JsError, JsValue, Promising,
    convert::{
        FromWasmAbi, IntoWasmAbi, OptionFromWasmAbi, OptionIntoWasmAbi, RefFromWasmAbi, WasmAbi,
    },
    wasm_bindgen,
};

#[wasm_bindgen]
extern "C" {
    type DefaultPromisingCompatType;

    #[wasm_bindgen(no_promising)]
    type ManualPromisingCompatType;
}

impl Promising for ManualPromisingCompatType {
    type Resolution = JsValue;
}

pub(crate) fn test_imported_type_promising_compat() {
    fn assert_default<T: Promising<Resolution = T>>() {}
    fn assert_manual<T: Promising<Resolution = JsValue>>() {}

    assert_default::<DefaultPromisingCompatType>();
    assert_manual::<ManualPromisingCompatType>();
}

pub(crate) fn test_convert_traits_are_marker_bounds() {
    fn assert_into<T: IntoWasmAbi>() {}
    fn assert_from<T: FromWasmAbi>() {}
    fn assert_option_into<T: OptionIntoWasmAbi>() {}
    fn assert_option_from<T: OptionFromWasmAbi>() {}
    fn assert_wasm<T: WasmAbi>() {}
    fn assert_ref<T: RefFromWasmAbi>() {}
    fn assert_error<T: std::error::Error>() {}

    assert_into::<JsValue>();
    assert_from::<JsValue>();
    assert_ref::<JsValue>();
    assert_wasm::<JsValue>();

    assert_into::<Option<u32>>();
    assert_from::<Option<u32>>();
    assert_option_into::<Option<u32>>();
    assert_option_from::<Option<u32>>();
    assert_wasm::<Option<u32>>();

    assert_error::<JsError>();
}

pub(crate) fn test_jsvalue_abi_ref_preserves_heap_ref() {
    #[wasm_bindgen(inline_js = r#"
        export function api_abi_object(label) {
            return { label };
        }

        export function api_abi_read_label(value) {
            return value.label;
        }
    "#)]
    extern "C" {
        fn api_abi_object(label: u32) -> JsValue;
        fn api_abi_read_label(value: &JsValue) -> u32;
    }

    let value = api_abi_object(271);
    let abi = (&value).into_abi();
    let cloned = unsafe { JsValue::ref_from_abi(abi) }.as_ref().clone();
    assert_eq!(api_abi_read_label(&value), 271);
    assert_eq!(api_abi_read_label(&cloned), 271);

    let owned = api_abi_object(314);
    let abi = owned.into_abi();
    let decoded = unsafe { JsValue::from_abi(abi) };
    assert_eq!(api_abi_read_label(&decoded), 314);
}

pub(crate) fn test_u128_try_from_bigint_preserves_range() {
    #[wasm_bindgen(inline_js = r#"
        export function api_u128_large_bigint() {
            return 1n << 80n;
        }

        export function api_u128_negative_bigint() {
            return -1n;
        }

        export function api_u128_too_large_bigint() {
            return 1n << 128n;
        }
    "#)]
    extern "C" {
        fn api_u128_large_bigint() -> JsValue;
        fn api_u128_negative_bigint() -> JsValue;
        fn api_u128_too_large_bigint() -> JsValue;
    }

    assert_eq!(
        u128::try_from(api_u128_large_bigint()).unwrap(),
        1_u128 << 80
    );
    assert!(u128::try_from(api_u128_negative_bigint()).is_err());
    assert!(u128::try_from(api_u128_too_large_bigint()).is_err());
}
