//! Thin wrappers over raw N-API calls, for converting values in bulk without napi-rs'
//! per-value type machinery.

use std::ffi::c_char;
use std::ptr;

use napi::sys;

pub type V = sys::napi_value;

/// Errors of these calls only happen with a broken environment (or a pending
/// exception); they surface as plain JS errors.
fn check(status: sys::napi_status, what: &str) -> napi::Result<()> {
    if status == sys::Status::napi_ok {
        Ok(())
    } else {
        Err(napi::Error::from_reason(format!("N-API call {what} failed (status {status})")))
    }
}

macro_rules! call {
    ($f:ident($($arg:expr),*)) => {
        // SAFETY: every call passes the env of the current callback and values
        // created in it; out-pointers point at initialized locals.
        check(unsafe { sys::$f($($arg),*) }, stringify!($f))
    };
}

#[derive(Clone, Copy)]
pub struct Js(pub sys::napi_env);

impl Js {
    // -- reading --------------------------------------------------------------------------

    pub fn type_of(self, v: V) -> napi::Result<i32> {
        let mut t = 0;
        call!(napi_typeof(self.0, v, &mut t))?;
        Ok(t)
    }

    pub fn is_array(self, v: V) -> napi::Result<bool> {
        let mut b = false;
        call!(napi_is_array(self.0, v, &mut b))?;
        Ok(b)
    }

    pub fn is_date(self, v: V) -> napi::Result<bool> {
        let mut b = false;
        call!(napi_is_date(self.0, v, &mut b))?;
        Ok(b)
    }

    pub fn instance_of(self, v: V, ctor: V) -> napi::Result<bool> {
        let mut b = false;
        call!(napi_instanceof(self.0, v, ctor, &mut b))?;
        Ok(b)
    }

    pub fn array_len(self, v: V) -> napi::Result<u32> {
        let mut n = 0;
        call!(napi_get_array_length(self.0, v, &mut n))?;
        Ok(n)
    }

    pub fn element(self, v: V, i: u32) -> napi::Result<V> {
        let mut out = ptr::null_mut();
        call!(napi_get_element(self.0, v, i, &mut out))?;
        Ok(out)
    }

    /// The items of a JS array.
    pub fn elements(self, v: V) -> napi::Result<Vec<V>> {
        let n = self.array_len(v)?;
        (0..n).map(|i| self.element(v, i)).collect()
    }

    pub fn f64(self, v: V) -> napi::Result<f64> {
        let mut n = 0.0;
        call!(napi_get_value_double(self.0, v, &mut n))?;
        Ok(n)
    }

    pub fn bool(self, v: V) -> napi::Result<bool> {
        let mut b = false;
        call!(napi_get_value_bool(self.0, v, &mut b))?;
        Ok(b)
    }

    /// A bigint as an i64, and whether it fit.
    pub fn bigint(self, v: V) -> napi::Result<(i64, bool)> {
        let (mut n, mut lossless) = (0i64, false);
        call!(napi_get_value_bigint_int64(self.0, v, &mut n, &mut lossless))?;
        Ok((n, lossless))
    }

    pub fn string(self, v: V) -> napi::Result<String> {
        let mut len = 0usize;
        call!(napi_get_value_string_utf8(self.0, v, ptr::null_mut(), 0, &mut len))?;
        let mut buf = vec![0u8; len + 1];
        call!(napi_get_value_string_utf8(self.0, v, buf.as_mut_ptr() as *mut c_char, len + 1, &mut len))?;
        buf.truncate(len);
        String::from_utf8(buf).map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// `String(v)`.
    pub fn coerce_string(self, v: V) -> napi::Result<String> {
        let mut s = ptr::null_mut();
        call!(napi_coerce_to_string(self.0, v, &mut s))?;
        self.string(s)
    }

    /// Milliseconds since the epoch of a `Date` (NaN when invalid).
    pub fn date_ms(self, v: V) -> napi::Result<f64> {
        let mut ms = 0.0;
        call!(napi_get_date_value(self.0, v, &mut ms))?;
        Ok(ms)
    }

    pub fn get(self, obj: V, name: &str) -> napi::Result<V> {
        let key = self.str(name)?;
        let mut out = ptr::null_mut();
        call!(napi_get_property(self.0, obj, key, &mut out))?;
        Ok(out)
    }

    /// `JSON.stringify(v)`; `None` when it gives `undefined` (functions, symbols).
    pub fn json_stringify(self, v: V) -> napi::Result<Option<String>> {
        let mut global = ptr::null_mut();
        call!(napi_get_global(self.0, &mut global))?;
        let json = self.get(global, "JSON")?;
        let stringify = self.get(json, "stringify")?;
        let mut out = ptr::null_mut();
        let args = [v];
        call!(napi_call_function(self.0, json, stringify, 1, args.as_ptr(), &mut out))?;
        if self.type_of(out)? == sys::ValueType::napi_string {
            Ok(Some(self.string(out)?))
        } else {
            Ok(None)
        }
    }

    // -- creating -------------------------------------------------------------------------

    pub fn null(self) -> napi::Result<V> {
        let mut out = ptr::null_mut();
        call!(napi_get_null(self.0, &mut out))?;
        Ok(out)
    }

    pub fn boolean(self, b: bool) -> napi::Result<V> {
        let mut out = ptr::null_mut();
        call!(napi_get_boolean(self.0, b, &mut out))?;
        Ok(out)
    }

    pub fn number(self, n: f64) -> napi::Result<V> {
        let mut out = ptr::null_mut();
        call!(napi_create_double(self.0, n, &mut out))?;
        Ok(out)
    }

    pub fn int(self, n: i32) -> napi::Result<V> {
        let mut out = ptr::null_mut();
        call!(napi_create_int32(self.0, n, &mut out))?;
        Ok(out)
    }

    pub fn bigint_from(self, n: i64) -> napi::Result<V> {
        let mut out = ptr::null_mut();
        call!(napi_create_bigint_int64(self.0, n, &mut out))?;
        Ok(out)
    }

    pub fn str(self, s: &str) -> napi::Result<V> {
        let mut out = ptr::null_mut();
        call!(napi_create_string_utf8(self.0, s.as_ptr() as *const c_char, s.len() as isize, &mut out))?;
        Ok(out)
    }

    pub fn date(self, ms: f64) -> napi::Result<V> {
        let mut out = ptr::null_mut();
        call!(napi_create_date(self.0, ms, &mut out))?;
        Ok(out)
    }

    pub fn array(self, len: usize) -> napi::Result<V> {
        let mut out = ptr::null_mut();
        call!(napi_create_array_with_length(self.0, len, &mut out))?;
        Ok(out)
    }

    pub fn set_element(self, arr: V, i: u32, v: V) -> napi::Result<()> {
        call!(napi_set_element(self.0, arr, i, v))
    }

    /// A JS array of `items`.
    pub fn array_of(self, items: impl ExactSizeIterator<Item = napi::Result<V>>) -> napi::Result<V> {
        let arr = self.array(items.len())?;
        for (i, v) in items.enumerate() {
            self.set_element(arr, i as u32, v?)?;
        }
        Ok(arr)
    }

    pub fn object(self) -> napi::Result<V> {
        let mut out = ptr::null_mut();
        call!(napi_create_object(self.0, &mut out))?;
        Ok(out)
    }

    pub fn set(self, obj: V, name: &str, v: V) -> napi::Result<()> {
        let key = self.str(name)?;
        call!(napi_set_property(self.0, obj, key, v))
    }

    /// `obj[name] = v` as a read-only property that `Object.keys`, `JSON.stringify` and
    /// `assert.deepEqual` do not see.
    pub fn define_hidden(self, obj: V, name: &str, v: V) -> napi::Result<()> {
        let desc = sys::napi_property_descriptor {
            utf8name: ptr::null(),
            name: self.str(name)?,
            method: None,
            getter: None,
            setter: None,
            value: v,
            attributes: sys::PropertyAttributes::default,
            data: ptr::null_mut(),
        };
        call!(napi_define_properties(self.0, obj, 1, &desc))
    }

    /// `new ctor(arg)`.
    pub fn construct(self, ctor: V, arg: V) -> napi::Result<V> {
        let mut out = ptr::null_mut();
        let args = [arg];
        call!(napi_new_instance(self.0, ctor, 1, args.as_ptr(), &mut out))?;
        Ok(out)
    }

    // -- references -----------------------------------------------------------------------

    pub fn reference(self, v: V) -> napi::Result<sys::napi_ref> {
        let mut out = ptr::null_mut();
        call!(napi_create_reference(self.0, v, 1, &mut out))?;
        Ok(out)
    }

    pub fn deref(self, r: sys::napi_ref) -> napi::Result<V> {
        let mut out = ptr::null_mut();
        call!(napi_get_reference_value(self.0, r, &mut out))?;
        Ok(out)
    }
}
