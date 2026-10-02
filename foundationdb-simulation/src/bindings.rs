//! Wrapper module
//!
//! This module defines all C and Rust structures.
//! It also provides bindings and wrappers to map behavior from Rust to C.

use std::{
    ffi::{self, c_char},
    str::FromStr,
    time::Duration,
};

use foundationdb as fdb;

mod raw_bindings {
    #![allow(non_camel_case_types)]
    #![allow(non_upper_case_globals)]
    #![allow(non_snake_case)]
    #![allow(dead_code)]
    #![allow(missing_docs)]
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}
pub use raw_bindings::{
    FDBDatabase, FDBMetrics, FDBPromise, FDBWorkload, FDBWorkloadContext, OpaqueWorkload,
};
use raw_bindings::{
    FDBMetric, FDBSeverity, FDBSeverity_FDBSeverity_Debug, FDBSeverity_FDBSeverity_Error,
    FDBSeverity_FDBSeverity_Info, FDBSeverity_FDBSeverity_Warn, FDBSeverity_FDBSeverity_WarnAlways,
    FDBStringPair,
};

pub use raw_bindings::FDBWorkload_FDBWorkload_VT as FDBWorkload_VT;
pub const FDB_WORKLOAD_API_VERSION: i32 = raw_bindings::FDB_WORKLOAD_API_VERSION as i32;

// -----------------------------------------------------------------------------
// String conversions

#[doc(hidden)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn str_from_c(c_buf: *const c_char) -> String {
    let c_str = unsafe { ffi::CStr::from_ptr(c_buf) };
    c_str.to_str().unwrap().to_string()
}
#[doc(hidden)]
pub fn str_for_c<T>(buf: T) -> ffi::CString
where
    T: Into<Vec<u8>>,
{
    let mut buf = buf.into();
    if buf.contains(&0) {
        let mut escaped = Vec::with_capacity(buf.len());
        for byte in buf {
            if byte == 0 {
                escaped.extend_from_slice(br"\0");
            } else {
                escaped.push(byte);
            }
        }
        buf = escaped;
    }

    // SAFETY: Every interior NUL byte was escaped above.
    unsafe { ffi::CString::from_vec_unchecked(buf) }
}

/// Capitalizes the first letter of a string.
/// Used to ensure trace detail names start with a capital letter.
/// Returns `None` if the string is empty.
fn capitalize_first(s: &str) -> Option<String> {
    let mut chars = s.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
}

/// ASCII-uppercases the first byte of a trace event name, matching FDB's convention that
/// event types start with a capital letter. Leaves an empty name unchanged.
fn capitalize_first_byte(mut name: Vec<u8>) -> Vec<u8> {
    if let Some(first) = name.first_mut() {
        first.make_ascii_uppercase();
    }
    name
}

/// Macro that can be used to create log "details" more easily.
#[macro_export]
macro_rules! details {
    ($($k:expr_2021 => $v:expr_2021),* $(,)?) => {
        &[
            $((
                &$k.to_string(), &$v.to_string()
            )),*
        ]
    };
}

// -----------------------------------------------------------------------------
// Rust Types

/// Wrapper around the C FDBWorkloadContext
pub struct WorkloadContext(FDBWorkloadContext);
/// Wrapper around the C FDBPromise
pub struct Promise(FDBPromise);
/// Wrapper around the C FDBMetrics
pub struct Metrics(FDBMetrics);

/// A single metric entry
#[derive(Clone)]
pub struct Metric<'a> {
    /// The name of the metric
    pub key: &'a str,
    /// The value of the metric
    pub val: f64,
    /// Indicates if the value represents an average or not
    pub avg: bool,
    /// C `printf` format for one `double`, defaulting to `%.3g`.
    ///
    /// Supported formats contain exactly one `a`, `A`, `e`, `E`, `f`, `F`, `g`,
    /// or `G` conversion, with optional `-`, `+`, space, `#`, and `0` flags and
    /// decimal width and precision no greater than `i32::MAX`. Literal text and
    /// `%%` are allowed. Length modifiers, positional arguments, `*`, and other
    /// conversions are rejected by [`Metrics::push`] before entering native code.
    pub fmt: Option<&'a str>,
}

/// Indicates the severity of a FoundationDB log entry
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum Severity {
    /// debug
    Debug = FDBSeverity_FDBSeverity_Debug,
    /// info
    Info = FDBSeverity_FDBSeverity_Info,
    /// warn
    Warn = FDBSeverity_FDBSeverity_Warn,
    /// warn always
    WarnAlways = FDBSeverity_FDBSeverity_WarnAlways,
    /// error, this severity automatically breaks execution. `WorkloadContext::trace` also
    /// appends a `RustFailure="1"` detail on top of the `RustWorkload="1"` detail added to
    /// every event, so trace consumers can tell a Rust workload failure apart from an
    /// FDB-internal Sev40 event.
    Error = FDBSeverity_FDBSeverity_Error,
}

// -----------------------------------------------------------------------------
// Implementations

macro_rules! with {
    ($this:expr_2021=>$method:ident($($args:expr_2021),* $(,)?)) => {
        unsafe { (*$this.vt).$method.unwrap_unchecked()($this.inner $(, $args)*) }
    };
}

impl Clone for WorkloadContext {
    /// Clones the wrapper by copying the underlying `FDBWorkloadContext` POD (a
    /// bundle of raw pointers owned by fdbserver, with no `Drop`). The clone
    /// aliases the same fdbserver-owned context, so it is only valid for the
    /// lifetime of the workload instance the original was handed to; once
    /// fdbserver frees that context, every clone dangles.
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

/// Detail key automatically appended to `Severity::Error` trace events.
const RUST_FAILURE_KEY: &str = "RustFailure";
/// Detail key automatically appended to every trace event, regardless of severity.
const RUST_WORKLOAD_KEY: &str = "RustWorkload";

/// Appends a `key="1"` detail to `details_storage` unless a detail with that key (already
/// capitalized) is present.
fn push_marker_if_absent(details_storage: &mut Vec<(ffi::CString, ffi::CString)>, key: &str) {
    if !details_storage
        .iter()
        .any(|(k, _)| k.as_bytes() == key.as_bytes())
    {
        details_storage.push((str_for_c(key), str_for_c("1")));
    }
}

/// Builds the trace detail storage for [`WorkloadContext::trace`].
///
/// Applies `capitalize_first` to every caller-supplied key (dropping empty values), then
/// appends a `RustWorkload="1"` detail to every event and, for [`Severity::Error`], a
/// `RustFailure="1"` detail as well, unless the caller already supplied one under that key.
fn prepare_trace_details<S2, S3>(
    severity: Severity,
    details: &[(S2, S3)],
) -> Vec<(ffi::CString, ffi::CString)>
where
    S2: AsRef<str>,
    S3: AsRef<str>,
{
    let mut details_storage = details
        .iter()
        .filter_map(|(key, val)| {
            let val = val.as_ref();
            if val.is_empty() {
                return None;
            }
            capitalize_first(key.as_ref()).map(|k| (str_for_c(k), str_for_c(val)))
        })
        .collect::<Vec<_>>();
    push_marker_if_absent(&mut details_storage, RUST_WORKLOAD_KEY);
    if matches!(severity, Severity::Error) {
        push_marker_if_absent(&mut details_storage, RUST_FAILURE_KEY);
    }
    details_storage
}

impl WorkloadContext {
    #[doc(hidden)]
    pub fn new(raw: FDBWorkloadContext) -> Self {
        Self(raw)
    }

    /// Get the server FDB_WORKLOAD_API_VERSION
    pub fn get_workload_api_version(&self) -> i32 {
        self.0.api_version
    }

    /// Add a log entry in the FoundationDB logs.
    ///
    /// The event `name`'s first byte is uppercased automatically, matching FDB's convention
    /// for event types. A `RustWorkload="1"` detail is appended to every event (unless the
    /// caller already provided one), so trace consumers can grep all Rust-origin trace lines
    /// with a single token. When `severity` is [`Severity::Error`], a `RustFailure="1"` detail
    /// is appended too (same rule), so a Rust-detected failure can be told apart from an
    /// FDB-internal Sev40 event.
    pub fn trace<S, S2, S3>(&self, severity: Severity, name: S, details: &[(S2, S3)])
    where
        S: Into<Vec<u8>>,
        S2: AsRef<str>,
        S3: AsRef<str>,
    {
        let name = str_for_c(capitalize_first_byte(name.into()));
        let details_storage = prepare_trace_details(severity, details);
        let details = details_storage
            .iter()
            .map(|(key, val)| FDBStringPair {
                key: key.as_ptr(),
                val: val.as_ptr(),
            })
            .collect::<Vec<_>>();
        with! {
            self.0 => trace(
                severity as FDBSeverity,
                name.as_ptr(),
                details.as_ptr(),
                details.len() as i32,
            )
        }
    }
    /// Get the process id of the workload
    pub fn get_process_id(&self) -> u64 {
        with! { self.0 => getProcessID() }
    }
    /// Set the process id of the workload
    pub fn set_process_id(&self, id: u64) {
        with! { self.0 => setProcessID(id) }
    }
    /// Get the current simulated time in seconds (starts at zero)
    pub fn now(&self) -> f64 {
        with! { self.0 => now() }
    }
    /// Get a determinist 32-bit random number
    pub fn rnd(&self) -> u32 {
        with! { self.0 => rnd() }
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
        let null = "";
        let name = str_for_c(name);
        let default_value = str_for_c(null);
        let raw_value = with! {
            self.0 => getOption(name.as_ptr(), default_value.as_ptr())
        };
        let value = str_from_c(raw_value.inner);
        with! { raw_value => free() };
        if value == null { None } else { Some(value) }
    }
    /// Get the client id of the workload
    pub fn client_id(&self) -> i32 {
        with! { self.0 => clientId() }
    }
    /// Get the client id of the workload
    pub fn client_count(&self) -> i32 {
        with! { self.0 => clientCount() }
    }
    /// Get a determinist 64-bit random number
    pub fn shared_random_number(&self) -> i64 {
        with! { self.0 => sharedRandomNumber() }
    }
    /// Return a future that will be ready after a given (simulated) duration
    pub fn delay(
        &self,
        duration: Duration,
    ) -> impl std::future::Future<Output = fdb::FdbResult<()>> + Send + Sync + 'static + use<> {
        let f = with! { self.0 => delay(duration.as_secs_f64()) };
        fdb::future::FdbFuture::new(f as *mut _)
    }
}

impl Promise {
    pub(crate) fn new(raw: FDBPromise) -> Self {
        Self(raw)
    }
    /// Resolve a FoundationDB promise by setting its value to a boolean.
    /// You can resolve a Promise only once.
    ///
    /// note: FoundationDB disregards the value sent, so sending `true` or `false` is equivalent
    pub fn send(self, value: bool) {
        with! { self.0 => send(value) };
    }
}
impl Drop for Promise {
    fn drop(&mut self) {
        with! { self.0 => free() };
    }
}

// Native metric formatting passes exactly one double to printf. Keep this
// grammar narrow so neither the conversion nor width/precision consumes a
// differently typed or additional variadic argument.
fn valid_metric_format(format: &str) -> bool {
    fn decimal(input: &mut &[u8]) -> bool {
        let mut value = 0_i32;
        while let Some((&digit, rest)) = input.split_first() {
            if !digit.is_ascii_digit() {
                break;
            }
            let Some(next) = value
                .checked_mul(10)
                .and_then(|value| value.checked_add(i32::from(digit - b'0')))
            else {
                return false;
            };
            value = next;
            *input = rest;
        }
        true
    }

    let mut input = format.as_bytes();
    let mut conversion = false;
    while let Some((&byte, rest)) = input.split_first() {
        input = rest;
        if byte != b'%' {
            continue;
        }
        if input.first() == Some(&b'%') {
            input = &input[1..];
            continue;
        }
        if conversion {
            return false;
        }
        while matches!(input.first(), Some(b'-' | b'+' | b' ' | b'#' | b'0')) {
            input = &input[1..];
        }
        if !decimal(&mut input) {
            return false;
        }
        if input.first() == Some(&b'.') {
            input = &input[1..];
            if !decimal(&mut input) {
                return false;
            }
        }
        match input.split_first() {
            Some((b'a' | b'A' | b'e' | b'E' | b'f' | b'F' | b'g' | b'G', rest)) => {
                conversion = true;
                input = rest;
            }
            _ => return false,
        }
    }
    conversion
}

impl Metrics {
    pub(crate) fn new(raw: FDBMetrics) -> Self {
        Self(raw)
    }
    /// Call std::vector::reserve on the underlying C++ sink
    pub fn reserve(&mut self, n: usize) {
        with! { self.0 => reserve(n as i32) }
    }
    /// Push a [Metric] entry in the underlying C++ sink.
    ///
    /// # Panics
    /// Panics before calling the native sink if [`Metric::fmt`] does not follow
    /// its supported single-double format grammar.
    pub fn push(&mut self, metric: Metric) {
        let format = metric.fmt.unwrap_or("%.3g");
        assert!(
            valid_metric_format(format),
            "invalid metric format: {format:?}"
        );
        let key_storage = str_for_c(metric.key);
        let fmt_storage = str_for_c(format);
        with! {
            self.0 => push(FDBMetric {
                key: key_storage.as_ptr(),
                fmt: fmt_storage.as_ptr(),
                val: metric.val,
                avg: metric.avg,
            })
        }
    }
    /// Push several [Metric] entries in the underlying C++ sink
    pub fn extend<'a, T>(&mut self, metrics: T)
    where
        T: IntoIterator<Item = Metric<'a>>,
    {
        let metrics = metrics.into_iter();
        let (min, max) = metrics.size_hint();
        self.reserve(max.unwrap_or(min));
        for metric in metrics {
            self.push(metric);
        }
    }
}

impl<'a> Metric<'a> {
    /// Create a metric value entry
    pub fn val<V>(key: &'a str, val: V) -> Self
    where
        V: TryInto<f64>,
    {
        Self {
            key,
            val: val.try_into().ok().expect("convertion failed"),
            avg: false,
            fmt: None,
        }
    }
    /// Create a metric average entry
    pub fn avg<V>(key: &'a str, val: V) -> Self
    where
        V: TryInto<f64>,
    {
        Self {
            key,
            val: val.try_into().ok().expect("convertion failed"),
            avg: true,
            fmt: None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn metric_formats_consume_exactly_one_double() {
        for format in [
            "%.3g",
            "%a",
            "%A",
            "%e",
            "%E",
            "%f",
            "%F",
            "%G",
            "load=%-+#010.3f%%",
            "% .f",
        ] {
            assert!(super::valid_metric_format(format), "rejected {format:?}");
        }
        for format in [
            "",
            "%%",
            "%n",
            "%s",
            "%d",
            "%p",
            "%Lf",
            "%lf",
            "%1$f",
            "%*f",
            "%.*f",
            "%f %g",
            "%f%",
            "%",
            "%.2.3f",
            "%2147483648f",
            "%.2147483648f",
            "%g\0%n",
        ] {
            assert!(!super::valid_metric_format(format), "accepted {format:?}");
        }
    }

    use super::{Severity, capitalize_first_byte, prepare_trace_details, str_for_c};

    #[test]
    fn str_for_c_escapes_interior_nul() {
        assert_eq!(str_for_c("a\0b").to_bytes(), br"a\0b");
    }

    #[test]
    fn error_severity_gets_rust_failure_marker() {
        let details: &[(&str, &str)] = &[("Reason", "boom")];
        let result = prepare_trace_details(Severity::Error, details);
        assert!(
            result
                .iter()
                .any(|(k, v)| k.as_bytes() == b"RustFailure" && v.as_bytes() == b"1")
        );
    }

    #[test]
    fn non_error_severity_has_no_rust_failure_marker() {
        let details: &[(&str, &str)] = &[("Reason", "boom")];
        let result = prepare_trace_details(Severity::Warn, details);
        assert!(!result.iter().any(|(k, _)| k.as_bytes() == b"RustFailure"));
    }

    #[test]
    fn caller_lowercase_rust_failure_key_is_not_duplicated() {
        let details: &[(&str, &str)] = &[("rustFailure", "custom")];
        let result = prepare_trace_details(Severity::Error, details);
        let matching: Vec<_> = result
            .iter()
            .filter(|(k, _)| k.as_bytes() == b"RustFailure")
            .collect();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].1.as_bytes(), b"custom");
    }

    #[test]
    fn caller_capitalized_rust_failure_key_is_not_duplicated() {
        let details: &[(&str, &str)] = &[("RustFailure", "custom")];
        let result = prepare_trace_details(Severity::Error, details);
        let matching: Vec<_> = result
            .iter()
            .filter(|(k, _)| k.as_bytes() == b"RustFailure")
            .collect();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].1.as_bytes(), b"custom");
    }

    #[test]
    fn every_severity_gets_rust_workload_marker() {
        let details: &[(&str, &str)] = &[];
        for severity in [
            Severity::Debug,
            Severity::Info,
            Severity::Warn,
            Severity::WarnAlways,
            Severity::Error,
        ] {
            let result = prepare_trace_details(severity, details);
            assert!(
                result
                    .iter()
                    .any(|(k, v)| k.as_bytes() == b"RustWorkload" && v.as_bytes() == b"1")
            );
        }
    }

    #[test]
    fn error_severity_gets_both_markers() {
        let details: &[(&str, &str)] = &[];
        let result = prepare_trace_details(Severity::Error, details);
        assert!(
            result
                .iter()
                .any(|(k, v)| k.as_bytes() == b"RustWorkload" && v.as_bytes() == b"1")
        );
        assert!(
            result
                .iter()
                .any(|(k, v)| k.as_bytes() == b"RustFailure" && v.as_bytes() == b"1")
        );
    }

    #[test]
    fn caller_lowercase_rust_workload_key_is_not_duplicated() {
        let details: &[(&str, &str)] = &[("rustWorkload", "custom")];
        let result = prepare_trace_details(Severity::Info, details);
        let matching: Vec<_> = result
            .iter()
            .filter(|(k, _)| k.as_bytes() == b"RustWorkload")
            .collect();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].1.as_bytes(), b"custom");
    }

    #[test]
    fn caller_capitalized_rust_workload_key_is_not_duplicated() {
        let details: &[(&str, &str)] = &[("RustWorkload", "custom")];
        let result = prepare_trace_details(Severity::Info, details);
        let matching: Vec<_> = result
            .iter()
            .filter(|(k, _)| k.as_bytes() == b"RustWorkload")
            .collect();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].1.as_bytes(), b"custom");
    }

    #[test]
    fn capitalize_first_byte_uppercases_lowercase_first_byte() {
        assert_eq!(capitalize_first_byte(b"event".to_vec()), b"Event".to_vec());
    }

    #[test]
    fn capitalize_first_byte_leaves_already_capitalized_name_unchanged() {
        assert_eq!(capitalize_first_byte(b"Event".to_vec()), b"Event".to_vec());
    }

    #[test]
    fn capitalize_first_byte_leaves_empty_name_unchanged() {
        assert_eq!(capitalize_first_byte(Vec::new()), Vec::new());
    }
}
