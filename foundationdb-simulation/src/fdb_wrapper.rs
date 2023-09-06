//! Wrapper module
//!
//! This module defines all C++ and Rust structures.
//! It also provides bindings and wrappers to map behavior from Rust to C++.

use std::{
    ffi::{CStr, CString},
    os::raw::c_char,
    str::FromStr,
};

// -----------------------------------------------------------------------------
// FFI safe "binding" structs

/// Contain all opaque types used by Rust to cover C++ types of unknown layout.
/// They are all equivalent to a `u8` and should always be used through a pointer or reference.
pub(crate) mod opaque {
    /// Opaque C++ type covering `std::string`
    #[repr(C)]
    pub(crate) struct String(u8);
    /// Opaque C++ type covering `std::vector<FDBPerfMetric>`
    #[repr(C)]
    pub(crate) struct Metrics(u8);
    /// Opaque C++ type covering `Opaque::FDBWorkloadContext` or `FDBLogger`
    #[repr(C)]
    pub(crate) struct Context(u8);
    /// Opaque C++ type covering `Wrapper<GenericPromise<bool>>`
    #[repr(C)]
    pub(crate) struct Promise(u8);
}

#[repr(C)]
struct CPPMetric {
    name: *const c_char,
    value: f64,
    averaged: bool,
    format_code: *const c_char,
}
#[repr(C)]
struct CPPStringPair {
    key: *const c_char,
    value: *const c_char,
}

// -----------------------------------------------------------------------------
// Rust interface structs

/// A wrapper around a FoundationDB promise
pub struct WorkloadContext {
    inner: *const opaque::Context,
}

/// A wrapper around a FoundationDB promise
pub struct Promise {
    inner: *const opaque::Promise,
}

/// A single metric entry
pub struct Metric {
    /// The name of the metric
    pub name: String,
    /// The value of the metric
    pub value: f64,
    /// Indicates if the value represents an average or not
    pub averaged: bool,
    /// C++ string formatter of the metric
    pub format_code: Option<String>,
}

/// Indicates the severity of a FoundationDB log entry
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum Severity {
    /// debug
    Debug = 0,
    /// info
    Info = 1,
    /// warn
    Warn = 2,
    /// warn always
    WarnAlways = 3,
    /// error, this severity automatically breaks execution
    Error = 4,
}

/// A vector of key value string pairs to pass to `WorkloadContext::trace`
pub type Details = Vec<(String, String)>;

/// Macro that can be used to create `Details` more easily.
///
/// ```rs
/// let details1 = vec![
///     ("key1".into(), "val1".into()),
///     ("key2".into(), format!("key{}", 2)),
/// ];
/// let details2 = details[
///     "key1" => "val1",
///     "key2" => format!("key{}", 2),
/// ];
/// assert_eq!(details1, details2);
/// ```
#[macro_export]
macro_rules! details {
    ($($k:expr => $v:expr),* $(,)?) => {
        vec![
            $((
                $k.to_string(), $v.to_string()
            )),*
        ]
    };
}

// -----------------------------------------------------------------------------
// Rust to C++ bindings

extern "C" {
    /// Redirect FoundationDB call to symbol "workloadFactory" (in Rust) to the C++
    /// symbol "CPPWorkloadFactory", it instantiate and returns a pointer to the C++
    /// class `RustWorkloadFactory`.
    ///
    /// This should only be called by `#[simulation_entrypoint]`.
    ///
    /// # Arguments
    ///
    /// * `logger` - A pointer to a FDBLogger.
    pub fn CPPWorkloadFactory(logger: *const u8) -> *const u8;

    fn FDBContext_trace(
        context: *const opaque::Context,
        severity: Severity,
        name: *const c_char,
        details: *const CPPStringPair,
        n: u32,
    );

    fn FDBContext_getProcessID(context: *const opaque::Context) -> u64;
    fn FDBContext_setProcessID(context: *const opaque::Context);
    fn FDBContext_now(context: *const opaque::Context) -> f64;
    fn FDBContext_rnd(context: *const opaque::Context) -> u32;
    fn FDBContext_getOption(
        context: *const opaque::Context,
        name: *const c_char,
        defaultValue: *const c_char,
        tmpString: *mut *const opaque::String,
    ) -> *const c_char;
    fn FDBContext_clientId(context: *const opaque::Context) -> usize;
    fn FDBContext_clientCount(context: *const opaque::Context) -> usize;
    fn FDBContext_sharedRandomNumber(context: *const opaque::Context) -> u64;

    fn FDBPromise_send(promise: *const opaque::Promise, value: bool);
    fn FDBPromise_free(promise: *const opaque::Promise);
    fn FDBString_free(string: *const opaque::String);

    fn FDBMetrics_extend(out: *const opaque::Metrics, metrics: *const CPPMetric, n: u32);
}

// -----------------------------------------------------------------------------
// C++ strings to Rust strings conversion

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub(crate) fn str_from_c(c_buf: *const c_char) -> String {
    let c_str = unsafe { CStr::from_ptr(c_buf) };
    c_str.to_str().expect("c_str").to_string()
}
pub(crate) fn str_for_c<T>(buf: T) -> CString
where
    T: Into<Vec<u8>>,
{
    CString::new(buf).expect("CString::new failed")
}

// -----------------------------------------------------------------------------
// Wrappers to map C++ behavior to Rust structs

impl WorkloadContext {
    pub(crate) fn new(inner: *const opaque::Context) -> Self {
        Self { inner }
    }
    /// Add a log entry in the FoundationDB logs
    pub fn trace<S>(&self, severity: Severity, name: S, details: Vec<(String, String)>)
    where
        S: Into<Vec<u8>>,
    {
        let name = str_for_c(name);
        let details_storage = details
            .into_iter()
            .map(|(key, value)| {
                let key = str_for_c(key);
                let value = str_for_c(value);
                (key, value)
            })
            .collect::<Vec<_>>();
        let details = details_storage
            .iter()
            .map(|(key, value)| CPPStringPair {
                key: key.as_ptr(),
                value: value.as_ptr(),
            })
            .collect::<Vec<_>>();
        unsafe {
            FDBContext_trace(
                self.inner,
                severity,
                name.as_ptr(),
                details.as_ptr(),
                details.len() as u32,
            );
        }
    }
    /// Get the process id of the workload
    pub fn get_process_id(&self) -> u64 {
        unsafe { FDBContext_getProcessID(self.inner) }
    }
    /// Set the process id of the workload
    pub fn set_process_id(&self) {
        unsafe { FDBContext_setProcessID(self.inner) }
    }
    /// Get the current time
    pub fn now(&self) -> f64 {
        unsafe { FDBContext_now(self.inner) }
    }
    /// Get a determinist 32-bit random number
    pub fn rnd(&self) -> u32 {
        unsafe { FDBContext_rnd(self.inner) }
    }
    /// Get the value of a parameter from the simulation config file
    ///
    /// /!\ getting an option consumes it, following call on that option will return `None`
    pub fn get_option<T>(&self, name: &str) -> Option<T>
    where
        T: FromStr,
    {
        self.get_option_raw(name)
            .and_then(|value| value.parse::<T>().ok())
    }
    fn get_option_raw(&self, name: &str) -> Option<String> {
        let null = "null";
        let name = str_for_c(name);
        let default_value = str_for_c(null);
        let mut cpp_tmp_string_ptr = std::ptr::null();
        let value_ptr = unsafe {
            FDBContext_getOption(
                self.inner,
                name.as_ptr(),
                default_value.as_ptr(),
                &mut cpp_tmp_string_ptr,
            )
        };
        let value = str_from_c(value_ptr);
        unsafe { FDBString_free(cpp_tmp_string_ptr) };
        if value == null {
            None
        } else {
            Some(value)
        }
    }
    /// Get the client id of the workload
    pub fn client_id(&self) -> usize {
        unsafe { FDBContext_clientId(self.inner) }
    }
    /// Get the client id of the workload
    pub fn client_count(&self) -> usize {
        unsafe { FDBContext_clientCount(self.inner) }
    }
    /// Get a determinist 64-bit random number
    pub fn shared_random_number(&self) -> u64 {
        unsafe { FDBContext_sharedRandomNumber(self.inner) }
    }
}

impl Promise {
    pub(crate) fn new(inner: *const opaque::Promise) -> Self {
        Self { inner }
    }
    /// Resolve a FoundationDB promise by setting its value to a boolean.
    /// You can resolve a Promise only once.
    ///
    /// note: FoundationDB disregards the value sent, so sending `true` or `false` is equivalent
    pub fn send(self, value: bool) {
        unsafe { FDBPromise_send(self.inner, value) };
    }
}
impl Drop for Promise {
    fn drop(&mut self) {
        unsafe { FDBPromise_free(self.inner) };
    }
}

impl Metric {
    /// Create a metric value entry
    pub fn val<S, V>(name: S, value: V) -> Self
    where
        S: Into<String>,
        f64: From<V>,
    {
        Self {
            name: name.into(),
            value: value.into(),
            averaged: false,
            format_code: None,
        }
    }
    /// Create a metric average entry
    pub fn avg<S, V>(name: S, value: V) -> Self
    where
        S: Into<String>,
        V: TryInto<f64>,
    {
        Self {
            name: name.into(),
            value: value.try_into().ok().expect("convertion failed"),
            averaged: true,
            format_code: None,
        }
    }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub(crate) fn metrics_extend(out: *const opaque::Metrics, metrics: Vec<Metric>) {
    let metrics_storage = metrics
        .into_iter()
        .map(
            |Metric {
                 name,
                 value,
                 averaged,
                 format_code,
             }| {
                let default_format_code = "0.3g".to_string();
                let name = str_for_c(name);
                let format_code = str_for_c(format_code.unwrap_or(default_format_code));
                (name, value, averaged, format_code)
            },
        )
        .collect::<Vec<_>>();
    let metrics = metrics_storage
        .iter()
        .map(|(name, value, averaged, format_code)| CPPMetric {
            name: name.as_ptr(),
            value: *value,
            averaged: *averaged,
            format_code: format_code.as_ptr(),
        })
        .collect::<Vec<_>>();
    unsafe { FDBMetrics_extend(out, metrics.as_ptr(), metrics.len() as u32) }
}
