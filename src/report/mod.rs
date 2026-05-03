//! Structured event reporting.

mod event;
mod sink;
mod stream;

pub use event::{BumpSource, Event, LeakDetail, TreeNode, TreeNodeKind};
pub use sink::EventSink;
pub use stream::StreamSink;
