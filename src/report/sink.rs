use crate::report::event::Event;

/// Receiver of structured events.
pub trait EventSink {
    /// Emit one event. Implementations must be safe to call from `&self`.
    fn emit(&self, event: &Event<'_>);
}
