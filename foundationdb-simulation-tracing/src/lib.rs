#![warn(missing_docs)]
#![doc = include_str!("../README.md")]

use std::{
    cell::RefCell,
    fmt,
    sync::{
        Once,
        atomic::{AtomicU64, Ordering},
    },
};

use foundationdb_simulation::{Severity, WorkloadContext};
use tracing_core::{
    Dispatch, Event, Level, Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};

// The context bound to the current simulation thread. `WorkloadContext` is
// `!Send`/`!Sync` (it wraps raw fdbserver pointers), so it never leaves the
// thread it was installed on: events fired on any other thread find an empty
// slot and are dropped. The slot also carries the install generation so an
// older `TracingGuard` cannot clear a binding that a newer `install` replaced.
thread_local! {
    static SIM_CONTEXT: RefCell<Option<(WorkloadContext, u64)>> = const { RefCell::new(None) };
}

/// Monotonic install counter used to tell competing bindings apart.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Guards the one-shot global subscriber registration.
static INIT: Once = Once::new();

/// Installs the global tracing subscriber (first call only, idempotent) and
/// binds tracing events emitted on the current simulation thread to
/// `context`'s trace log. Call from your workload factory
/// (`SingleRustWorkload::new` / `RustWorkloadFactory::create`) and store the
/// returned guard in your workload struct: when the guard drops (workload
/// teardown), the binding is cleared, so no event can ever reach a freed
/// context.
///
/// If another global tracing subscriber is already installed, this prints a
/// warning to stderr and forwarding stays disabled (events reach the
/// pre-existing subscriber instead).
// Not `#[instrument]`: the crate depends only on `tracing-core`, which has no
// attribute macro (`#[instrument]` lives in `tracing`), and on the first call
// the subscriber does not exist yet, so there would be nothing to record.
#[must_use = "dropping the guard immediately unbinds the context, so tracing \
              events would no longer be forwarded"]
pub fn install(context: &WorkloadContext) -> TracingGuard {
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    SIM_CONTEXT.with(|slot| {
        *slot.borrow_mut() = Some((context.clone(), generation));
    });
    INIT.call_once(|| {
        let dispatch = Dispatch::new(SimSubscriber::default());
        if tracing_core::dispatcher::set_global_default(dispatch).is_err() {
            eprintln!(
                "foundationdb-simulation-tracing: a global tracing subscriber is already \
                 installed; simulator trace forwarding is disabled and events will reach the \
                 pre-existing subscriber instead"
            );
        }
    });
    TracingGuard { generation }
}

/// Clears the thread-local context slot on drop, but only if the slot still
/// points at the context this guard was created for (a newer [`install`]
/// wins). Keep it alive for as long as tracing events should be forwarded,
/// typically by storing it in your workload struct.
pub struct TracingGuard {
    generation: u64,
}

impl Drop for TracingGuard {
    fn drop(&mut self) {
        SIM_CONTEXT.with(|slot| {
            let mut slot = slot.borrow_mut();
            if matches!(&*slot, Some((_, generation)) if *generation == self.generation) {
                *slot = None;
            }
        });
    }
}

/// Bare `tracing_core::Subscriber` that only reacts to events: spans get a
/// unique id but are otherwise ignored, and every event is forwarded to the
/// thread-local simulation context. Holds no `WorkloadContext`, so it stays
/// `Send + Sync` as `Dispatch` requires.
#[derive(Default)]
struct SimSubscriber {
    next_span_id: AtomicU64,
}

impl Subscriber for SimSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        // span ids must be non-zero
        Id::from_u64(self.next_span_id.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        forward_event(event);
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

/// Forwards a single event to the bound context, or does nothing when no
/// context is bound to the current thread.
fn forward_event(event: &Event<'_>) {
    SIM_CONTEXT.with(|slot| {
        let slot = slot.borrow();
        let Some((context, _)) = slot.as_ref() else {
            return;
        };
        let metadata = event.metadata();
        let mut visitor = DetailVisitor::default();
        event.record(&mut visitor);

        let mut details: Vec<(String, String)> = Vec::with_capacity(visitor.fields.len() + 4);
        details.push(("Target".to_owned(), sanitize(metadata.target())));
        if let Some(file) = metadata.file() {
            details.push(("File".to_owned(), sanitize(file)));
        }
        if let Some(line) = metadata.line() {
            details.push(("Line".to_owned(), line.to_string()));
        }
        if let Some(message) = visitor.message {
            details.push(("Message".to_owned(), message));
        }
        details.extend(visitor.fields);

        context.trace(severity(metadata.level()), "RustTracingEvent", &details);
    });
}

/// Maps a `tracing` level to an FDB [`Severity`]. `ERROR` maps to
/// [`Severity::WarnAlways`] rather than [`Severity::Error`] on purpose:
/// `Severity::Error` flags the whole simulation as failed, and a dependency's
/// `tracing::error!` must not kill a run. Failing a simulation stays an
/// explicit `context.trace(Severity::Error, ...)`.
fn severity(level: &Level) -> Severity {
    if *level == Level::ERROR {
        Severity::WarnAlways
    } else if *level == Level::WARN {
        Severity::Warn
    } else if *level == Level::INFO {
        Severity::Info
    } else {
        // TRACE and DEBUG
        Severity::Debug
    }
}

/// Strips NUL bytes so values survive the `CString` boundary in
/// `WorkloadContext::trace` (a NUL would otherwise panic across `extern "C"`).
fn sanitize(value: &str) -> String {
    if value.contains('\0') {
        value.chars().filter(|&c| c != '\0').collect()
    } else {
        value.to_owned()
    }
}

/// Collects an event's fields into `(key, value)` strings, pulling the field
/// literally named `message` out separately. Every key and value has NUL
/// bytes stripped before it is stored.
#[derive(Default)]
struct DetailVisitor {
    message: Option<String>,
    fields: Vec<(String, String)>,
}

impl DetailVisitor {
    fn push(&mut self, field: &Field, value: String) {
        let value = sanitize(&value);
        if field.name() == "message" {
            self.message = Some(value);
        } else {
            self.fields.push((sanitize(field.name()), value));
        }
    }
}

impl Visit for DetailVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field, format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value.to_owned());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field, value.to_string());
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        self.push(field, value.to_string());
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.push(field, value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push(field, value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field, value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use foundationdb_simulation::WorkloadContext;
    use foundationdb_simulation::internals::FDBWorkloadContext;
    use tracing_core::{Dispatch, Event, span::Id};

    use super::*;

    /// A zeroed context is enough for guard tests: we only store it in the slot
    /// and drop it, never calling any method that would dereference a pointer.
    fn dummy_context() -> WorkloadContext {
        WorkloadContext::new(unsafe { std::mem::zeroed::<FDBWorkloadContext>() })
    }

    fn slot_generation() -> Option<u64> {
        SIM_CONTEXT.with(|slot| slot.borrow().as_ref().map(|(_, generation)| *generation))
    }

    #[test]
    fn severity_mapping_never_errors() {
        assert!(matches!(severity(&Level::TRACE), Severity::Debug));
        assert!(matches!(severity(&Level::DEBUG), Severity::Debug));
        assert!(matches!(severity(&Level::INFO), Severity::Info));
        assert!(matches!(severity(&Level::WARN), Severity::Warn));
        assert!(matches!(severity(&Level::ERROR), Severity::WarnAlways));
    }

    #[test]
    fn sanitize_strips_nul_bytes() {
        assert_eq!(sanitize("a\0b\0c"), "abc");
        assert_eq!(sanitize("clean"), "clean");
        assert_eq!(sanitize("\0"), "");
    }

    /// Captures what `DetailVisitor` extracts from a real `tracing` event.
    #[derive(Clone, Default)]
    struct Captured {
        message: Option<String>,
        fields: Vec<(String, String)>,
    }

    struct CaptureSubscriber(Arc<Mutex<Option<Captured>>>);

    impl Subscriber for CaptureSubscriber {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _span: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _span: &Id, _values: &Record<'_>) {}
        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut visitor = DetailVisitor::default();
            event.record(&mut visitor);
            *self.0.lock().unwrap() = Some(Captured {
                message: visitor.message,
                fields: visitor.fields,
            });
        }
        fn enter(&self, _span: &Id) {}
        fn exit(&self, _span: &Id) {}
    }

    fn capture(emit: impl FnOnce()) -> Captured {
        let captured = Arc::new(Mutex::new(None));
        let subscriber = CaptureSubscriber(captured.clone());
        tracing_core::dispatcher::with_default(&Dispatch::new(subscriber), emit);
        captured.lock().unwrap().take().unwrap()
    }

    #[test]
    fn visitor_splits_message_from_fields() {
        let captured = capture(|| tracing::info!(client = 3, "hello world"));
        assert_eq!(captured.message.as_deref(), Some("hello world"));
        assert!(
            captured
                .fields
                .iter()
                .any(|(key, value)| key == "client" && value == "3")
        );
    }

    #[test]
    fn visitor_treats_explicit_message_field_as_message() {
        let captured = capture(|| tracing::info!(message = "explicit", other = 1));
        assert_eq!(captured.message.as_deref(), Some("explicit"));
        assert!(!captured.fields.iter().any(|(key, _)| key == "message"));
        assert!(
            captured
                .fields
                .iter()
                .any(|(key, value)| key == "other" && value == "1")
        );
    }

    #[test]
    fn visitor_strips_nul_from_keys_and_values() {
        let captured = capture(|| tracing::info!(dirty = "a\0b", "mes\0sage"));
        assert_eq!(captured.message.as_deref(), Some("message"));
        assert!(
            captured
                .fields
                .iter()
                .any(|(key, value)| key == "dirty" && value == "ab")
        );
    }

    #[test]
    fn guard_clears_slot_on_drop() {
        let context = dummy_context();
        {
            let _guard = install(&context);
            assert!(slot_generation().is_some());
        }
        assert_eq!(slot_generation(), None);
    }

    #[test]
    fn newer_install_survives_older_guard_drop() {
        let context = dummy_context();
        let guard_a = install(&context);
        let gen_a = slot_generation();
        let guard_b = install(&context);
        let gen_b = slot_generation();
        assert_ne!(gen_a, gen_b);

        // The older guard must leave the newer binding untouched.
        drop(guard_a);
        assert_eq!(slot_generation(), gen_b);

        // The newer guard owns the current binding and clears it.
        drop(guard_b);
        assert_eq!(slot_generation(), None);
    }
}
