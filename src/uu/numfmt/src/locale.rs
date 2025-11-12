// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::OnceLock;

const EMPTY_LOCALE: &[u8] = b"\0";

/// Information extracted from the active C locale that helps
/// numfmt parse and print localized numbers.
#[derive(Clone, Debug)]
pub struct NumericLocale {
    decimal_point: char,
    grouping_sep: String,
}

impl NumericLocale {
    pub fn decimal_point(&self) -> char {
        self.decimal_point
    }

    pub fn grouping_sep(&self) -> &str {
        &self.grouping_sep
    }
}

static NUMERIC_LOCALE: OnceLock<NumericLocale> = OnceLock::new();

pub fn get_numeric_locale() -> &'static NumericLocale {
    NUMERIC_LOCALE.get_or_init(load_numeric_locale)
}

fn load_numeric_locale() -> NumericLocale {
    set_locale_to_env();

    let locale_columns = unsafe { libc::localeconv() };
    let decimal_point = locale_string(unsafe { (*locale_columns).decimal_point });
    let grouping_sep = locale_string(unsafe { (*locale_columns).thousands_sep });

    let decimal_char = decimal_point.chars().next().unwrap_or('.');

    NumericLocale {
        decimal_point: decimal_char,
        grouping_sep,
    }
}

fn locale_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(not(target_os = "wasi"))]
fn set_locale_to_env() {
    unsafe {
        libc::setlocale(libc::LC_ALL, EMPTY_LOCALE.as_ptr() as *const c_char);
    }
}

#[cfg(target_os = "wasi")]
fn set_locale_to_env() {}
