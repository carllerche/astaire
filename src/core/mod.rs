pub use self::cell::{ActorCell, Cell, CellPtr};
pub use self::runtime::{Runtime, RuntimeWeak};
pub use self::scheduler::{Scheduler, currently_scheduled};
use self::future::{Async, Request};
use std::fmt;

pub mod future;

mod cell;
mod runtime;

#[cfg(ndebug)]
mod scheduler;

#[cfg(not(ndebug))]
#[path = "scheduler_dev.rs"]
mod scheduler;

enum Event<M: Send, R: Async> {
    Message(Request<M, R>),
    Spawn,
    Exec(Box<FnOnce<(),()> + Send>),
}

impl<M: Send, R: Async> Event<M, R> {
    fn message(message: Request<M, R>) -> Event<M, R> {
        Message(message)
    }

    fn exec(f: Box<FnOnce<(),()> + Send>) -> Event<M, R> {
        Exec(f)
    }

    fn is_message(&self) -> bool {
        match *self {
            Message(..) => true,
            _ => false,
        }
    }

    fn is_spawn(&self) -> bool {
        match *self {
            Spawn => true,
            _ => false,
        }
    }
}

impl<M: Send, R: Async> fmt::Show for Event<M, R> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Message(..) => write!(fmt, "Message"),
            Exec(..) => write!(fmt, "Exec"),
            Spawn => write!(fmt, "Spawn"),
        }
    }
}
