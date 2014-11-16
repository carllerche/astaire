pub use self::unsafe_arc::UnsafeArc;
pub use core::future::Async;

mod unsafe_arc;

pub mod future {
    pub use core::future::{Future, Completer};
}
