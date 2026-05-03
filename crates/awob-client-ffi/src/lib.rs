//! C ABI for `awob-client`.
//!
//! Produces `libawob_client.{so,dylib,dll}` and `libawob_client.a` along with
//! a generated `awob_client.h` header (via `cbindgen`).
//!
//! Clippy disables: `not_unsafe_ptr_arg_deref` and `missing_safety_doc` are
//! by-design for FFI bindings — every entry point takes raw pointers and
//! is intentionally callable from C without `unsafe` ceremony on the C
//! side. Each function's contract is documented in `awob_client.h`.
#![allow(clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
//!
//! Memory model:
//! * All allocations stay on the Rust side. The C caller hands us null-
//!   terminated `char*` strings; we copy them. Returned strings (last error)
//!   are valid until the next FFI call on the same thread.
//! * Handles are opaque pointers. Free with `awob_client_free` /
//!   `awob_send_free`.
//!
//! Threading: each handle is single-threaded. Concurrent calls on the same
//! handle from multiple threads are UB. Different handles are independent.

#![allow(non_camel_case_types)]

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char};
use std::os::raw::c_int;
use std::path::Path;
use std::ptr;

use awob_client::{Client, Send};

pub const AWOB_OK: c_int = 0;
pub const AWOB_ERR_BAD_ARG: c_int = 1;
pub const AWOB_ERR_CONNECT: c_int = 2;
pub const AWOB_ERR_IO: c_int = 3;
pub const AWOB_ERR_PROTOCOL: c_int = 4;
pub const AWOB_ERR_DAEMON: c_int = 5;

pub const AWOB_PROTOCOL_VERSION: u32 = awob_client::PROTOCOL_VERSION;
pub const AWOB_ABI_VERSION: u32 = 0;

pub struct awob_client_t {
    inner: Client,
}
pub struct awob_send_t {
    inner: Send,
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(msg: impl Into<String>) {
    let s =
        CString::new(msg.into()).unwrap_or_else(|_| CString::new("invalid error message").unwrap());
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(s));
}

fn clear_last_error() {
    LAST_ERROR.with(|e| *e.borrow_mut() = None);
}

unsafe fn cstr_to_string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// Returns the last error message set by any FFI call on this thread.
/// Pointer is valid until the next FFI call. Returns `NULL` when there is
/// no error to report.
#[unsafe(no_mangle)]
pub extern "C" fn awob_last_error() -> *const c_char {
    LAST_ERROR.with(|e| match &*e.borrow() {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    })
}

/// Returns the wire protocol version this build implements.
#[unsafe(no_mangle)]
pub extern "C" fn awob_protocol_version() -> u32 {
    AWOB_PROTOCOL_VERSION
}

/// Returns the ABI version of this shared library.
#[unsafe(no_mangle)]
pub extern "C" fn awob_abi_version() -> u32 {
    AWOB_ABI_VERSION
}

/// Connect to the daemon at the default socket path
/// (`$XDG_RUNTIME_DIR/awob.sock`). Returns NULL on failure; check
/// `awob_last_error()` for details.
#[unsafe(no_mangle)]
pub extern "C" fn awob_connect() -> *mut awob_client_t {
    clear_last_error();
    match Client::connect() {
        Ok(c) => Box::into_raw(Box::new(awob_client_t { inner: c })),
        Err(e) => {
            set_last_error(e.to_string());
            ptr::null_mut()
        }
    }
}

/// Connect to the daemon at an explicit socket path. `socket_path` must be a
/// valid null-terminated UTF-8 string. Returns NULL on failure.
#[unsafe(no_mangle)]
pub extern "C" fn awob_connect_to(socket_path: *const c_char) -> *mut awob_client_t {
    clear_last_error();
    let Some(path) = (unsafe { cstr_to_string(socket_path) }) else {
        set_last_error("socket_path is null");
        return ptr::null_mut();
    };
    match Client::connect_to(Path::new(&path)) {
        Ok(c) => Box::into_raw(Box::new(awob_client_t { inner: c })),
        Err(e) => {
            set_last_error(e.to_string());
            ptr::null_mut()
        }
    }
}

/// Free a client handle previously returned by `awob_connect*`.
#[unsafe(no_mangle)]
pub extern "C" fn awob_client_free(client: *mut awob_client_t) {
    if !client.is_null() {
        unsafe {
            drop(Box::from_raw(client));
        }
    }
}

/// Allocate a new send builder. `event` is required; `value` is the numeric
/// payload value. Caller must `awob_send_free` or pass to
/// `awob_send_dispatch` (which consumes the handle).
#[unsafe(no_mangle)]
pub extern "C" fn awob_send_new(event: *const c_char, value: f64) -> *mut awob_send_t {
    clear_last_error();
    let Some(ev) = (unsafe { cstr_to_string(event) }) else {
        set_last_error("event is null");
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(awob_send_t {
        inner: Send::new(ev, value),
    }))
}

/// Free a send builder without dispatching.
#[unsafe(no_mangle)]
pub extern "C" fn awob_send_free(send: *mut awob_send_t) {
    if !send.is_null() {
        unsafe {
            drop(Box::from_raw(send));
        }
    }
}

macro_rules! define_string_setter {
    ($fn_name:ident, $method:ident) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn $fn_name(send: *mut awob_send_t, value: *const c_char) -> c_int {
            clear_last_error();
            if send.is_null() {
                set_last_error("send is null");
                return AWOB_ERR_BAD_ARG;
            }
            let Some(s) = (unsafe { cstr_to_string(value) }) else {
                set_last_error("value is null");
                return AWOB_ERR_BAD_ARG;
            };
            let send = unsafe { &mut *send };
            let inner = std::mem::replace(&mut send.inner, Send::new("", 0.0));
            send.inner = inner.$method(s);
            AWOB_OK
        }
    };
}

define_string_setter!(awob_send_set_source, source);
define_string_setter!(awob_send_set_style, style);
define_string_setter!(awob_send_set_accent, accent);
define_string_setter!(awob_send_set_app, app);
define_string_setter!(awob_send_set_icon, icon);

/// Set `max` on the builder.
#[unsafe(no_mangle)]
pub extern "C" fn awob_send_set_max(send: *mut awob_send_t, max: f64) -> c_int {
    clear_last_error();
    if send.is_null() {
        set_last_error("send is null");
        return AWOB_ERR_BAD_ARG;
    }
    let send = unsafe { &mut *send };
    let inner = std::mem::replace(&mut send.inner, Send::new("", 0.0));
    send.inner = inner.max(max);
    AWOB_OK
}

/// Set `timeout_ms` on the builder.
#[unsafe(no_mangle)]
pub extern "C" fn awob_send_set_timeout_ms(send: *mut awob_send_t, ms: u32) -> c_int {
    clear_last_error();
    if send.is_null() {
        set_last_error("send is null");
        return AWOB_ERR_BAD_ARG;
    }
    let send = unsafe { &mut *send };
    let inner = std::mem::replace(&mut send.inner, Send::new("", 0.0));
    send.inner = inner.timeout_ms(ms);
    AWOB_OK
}

/// Dispatch the send. Consumes `send` regardless of outcome (you must NOT
/// call `awob_send_free` after this). Returns `AWOB_OK` or one of the
/// `AWOB_ERR_*` codes; `awob_last_error()` returns a description.
#[unsafe(no_mangle)]
pub extern "C" fn awob_send_dispatch(client: *mut awob_client_t, send: *mut awob_send_t) -> c_int {
    clear_last_error();
    if client.is_null() {
        set_last_error("client is null");
        return AWOB_ERR_BAD_ARG;
    }
    if send.is_null() {
        set_last_error("send is null");
        return AWOB_ERR_BAD_ARG;
    }
    let client = unsafe { &mut *client };
    let send = unsafe { Box::from_raw(send) };
    match client.inner.send(send.inner.build()) {
        Ok(()) => AWOB_OK,
        Err(e) => {
            let code = match &e {
                awob_client::Error::Io(_) => AWOB_ERR_IO,
                awob_client::Error::SocketMissing(_) | awob_client::Error::NoRuntimeDir => {
                    AWOB_ERR_CONNECT
                }
                awob_client::Error::Daemon(_) => AWOB_ERR_DAEMON,
                _ => AWOB_ERR_PROTOCOL,
            };
            set_last_error(e.to_string());
            code
        }
    }
}

/// Negotiate protocol version. Returns `AWOB_OK` on match, `AWOB_ERR_PROTOCOL`
/// if mismatched.
#[unsafe(no_mangle)]
pub extern "C" fn awob_hello(client: *mut awob_client_t) -> c_int {
    clear_last_error();
    if client.is_null() {
        set_last_error("client is null");
        return AWOB_ERR_BAD_ARG;
    }
    let client = unsafe { &mut *client };
    match client.inner.hello() {
        Ok(_) => AWOB_OK,
        Err(e) => {
            set_last_error(e.to_string());
            AWOB_ERR_PROTOCOL
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_constants() {
        assert_eq!(awob_protocol_version(), awob_client::PROTOCOL_VERSION);
        assert_eq!(awob_abi_version(), 0);
    }

    #[test]
    fn null_send_dispatch_returns_bad_arg() {
        let r = awob_send_dispatch(ptr::null_mut(), ptr::null_mut());
        assert_eq!(r, AWOB_ERR_BAD_ARG);
    }

    #[test]
    fn last_error_starts_empty() {
        clear_last_error();
        assert!(awob_last_error().is_null());
    }
}
