//! JavaScript system value wrappers and promise compatibility traits.

use core::{fmt, marker::PhantomData, ops::Deref};

use crate::{
    ErasableGeneric, IntoJsGeneric, JsCast, JsError, JsGeneric, JsValue, convert::UpcastFrom,
};

/// Marker trait for values that are either a resolution value or a promise-like value.
pub trait Promising {
    type Resolution;
}

macro_rules! js_primitive {
        ($name:ident, $const_name:ident, $value:expr, $display:literal, $check:expr) => {
            #[derive(Clone, PartialEq)]
            #[repr(transparent)]
            pub struct $name {
                pub obj: JsValue,
            }

            impl $name {
                pub const $const_name: $name = Self { obj: $value };
            }

            impl Eq for $name {}

            impl Default for $name {
                fn default() -> Self {
                    Self::$const_name
                }
            }

            impl fmt::Debug for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str($display)
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str($display)
                }
            }

            impl AsRef<JsValue> for $name {
                fn as_ref(&self) -> &JsValue {
                    &self.obj
                }
            }

            impl From<$name> for JsValue {
                fn from(value: $name) -> JsValue {
                    value.obj
                }
            }

            impl From<JsValue> for $name {
                fn from(obj: JsValue) -> Self {
                    Self { obj }
                }
            }

            impl JsCast for $name {
                fn instanceof(value: &JsValue) -> bool {
                    $check(value)
                }

                fn is_type_of(value: &JsValue) -> bool {
                    $check(value)
                }

                fn unchecked_from_js(value: JsValue) -> Self {
                    Self { obj: value }
                }

                fn unchecked_from_js_ref(value: &JsValue) -> &Self {
                    unsafe { &*(value as *const JsValue as *const Self) }
                }
            }

            impl_js_value_wire!(for $name, field obj);

            unsafe impl crate::__rt::marker::ErasableGeneric for $name {
                type Repr = JsValue;
            }

            impl IntoJsGeneric for $name {
                type JsCanon = Self;

                fn to_js(self) -> Self {
                    self
                }
            }

            impl UpcastFrom<$name> for $name {}
            impl UpcastFrom<$name> for JsValue {}
        };
    }

js_primitive!(
    Undefined,
    UNDEFINED,
    JsValue::UNDEFINED,
    "undefined",
    JsValue::is_undefined
);
js_primitive!(Null, NULL, JsValue::NULL, "null", JsValue::is_null);

impl UpcastFrom<()> for Undefined {}
impl UpcastFrom<Undefined> for () {}
impl UpcastFrom<()> for JsValue {}
impl UpcastFrom<()> for () {}

#[derive(Clone, PartialEq)]
#[repr(transparent)]
pub struct JsOption<T = JsValue> {
    pub obj: JsValue,
    pub generics: PhantomData<fn() -> T>,
}

impl<T: JsGeneric> JsOption<T> {
    #[inline]
    pub fn new() -> Self {
        Undefined::UNDEFINED.unchecked_into()
    }

    #[inline]
    pub fn wrap(val: T) -> Self {
        val.unchecked_into()
    }

    #[inline]
    pub fn from_option(opt: Option<T>) -> Self {
        match opt {
            Some(value) => Self::wrap(value),
            None => Self::new(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.obj.is_null() || self.obj.is_undefined()
    }

    #[inline]
    pub fn as_option(&self) -> Option<T> {
        if self.is_empty() {
            None
        } else {
            Some(T::unchecked_from_js(self.obj.clone()))
        }
    }

    #[inline]
    pub fn into_option(self) -> Option<T> {
        if self.is_empty() {
            None
        } else {
            Some(self.unchecked_into())
        }
    }

    #[inline]
    pub fn unwrap(self) -> T {
        self.expect("called `JsOption::unwrap()` on an empty value")
    }

    #[inline]
    pub fn expect(self, msg: &str) -> T {
        match self.into_option() {
            Some(value) => value,
            None => panic!("{}", msg),
        }
    }

    #[inline]
    pub fn unwrap_or_default(self) -> T
    where
        T: Default,
    {
        self.into_option().unwrap_or_default()
    }

    #[inline]
    pub fn unwrap_or_else<F>(self, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        self.into_option().unwrap_or_else(f)
    }
}

impl<T: JsGeneric> Default for JsOption<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: JsGeneric + fmt::Debug> fmt::Debug for JsOption<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}?(", core::any::type_name::<T>())?;
        match self.as_option() {
            Some(value) => write!(f, "{value:?}")?,
            None => f.write_str("null")?,
        }
        f.write_str(")")
    }
}

impl<T: JsGeneric + fmt::Display> fmt::Display for JsOption<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}?(", core::any::type_name::<T>())?;
        match self.as_option() {
            Some(value) => write!(f, "{value}")?,
            None => f.write_str("null")?,
        }
        f.write_str(")")
    }
}

impl<T> AsRef<JsValue> for JsOption<T> {
    fn as_ref(&self) -> &JsValue {
        &self.obj
    }
}

impl<T: JsGeneric> Deref for JsOption<T> {
    type Target = T;

    fn deref(&self) -> &T {
        T::unchecked_from_js_ref(&self.obj)
    }
}

impl<T> From<JsOption<T>> for JsValue {
    fn from(value: JsOption<T>) -> JsValue {
        value.obj
    }
}

impl<T> From<JsValue> for JsOption<T> {
    fn from(obj: JsValue) -> Self {
        Self {
            obj,
            generics: PhantomData,
        }
    }
}

impl<T: JsGeneric> JsCast for JsOption<T> {
    fn instanceof(value: &JsValue) -> bool {
        T::is_type_of(value) || value.is_null() || value.is_undefined()
    }

    fn unchecked_from_js(value: JsValue) -> Self {
        value.into()
    }

    fn unchecked_from_js_ref(value: &JsValue) -> &Self {
        unsafe { &*(value as *const JsValue as *const Self) }
    }
}

impl_js_value_wire!(impl<T> for JsOption<T>, field obj);

unsafe impl<T> crate::__rt::marker::ErasableGeneric for JsOption<T> {
    type Repr = JsValue;
}

impl<T: JsGeneric> IntoJsGeneric for JsOption<T> {
    type JsCanon = Self;

    fn to_js(self) -> Self {
        self
    }
}

impl UpcastFrom<JsValue> for JsOption<JsValue> {}
impl<T> UpcastFrom<Undefined> for JsOption<T> {}
impl<T> UpcastFrom<Null> for JsOption<T> {}
impl<T> UpcastFrom<()> for JsOption<T> {}
impl<T> UpcastFrom<JsOption<T>> for JsValue {}
impl<T, U> UpcastFrom<JsOption<U>> for JsOption<T> where T: UpcastFrom<U> {}

impl Promising for JsValue {
    type Resolution = JsValue;
}

impl Promising for () {
    type Resolution = Undefined;
}

macro_rules! promising_self {
        ($($ty:ty),* $(,)?) => {
            $(
                impl Promising for $ty {
                    type Resolution = $ty;
                }
            )*
        };
    }

promising_self!(
    bool, char, f32, f64, i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize, JsError
);

impl<T: Promising> Promising for Option<T> {
    type Resolution = Option<T::Resolution>;
}

impl<T: ErasableGeneric + Promising, E: ErasableGeneric> Promising for Result<T, E> {
    type Resolution = Result<T::Resolution, E>;
}
