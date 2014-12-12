pub use self::Event::*;
pub use self::SysEvent::*;
pub use self::cell::{Cell, CellRef};
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
    Exec(Box<FnOnce<(), ()> + Send>),
}

enum SysEvent {
    Spawn,
    Link(CellRef),
    Terminate,
    ChildTerminated(CellRef),
}

impl<M: Send, R: Async> Event<M, R> {
    fn message(message: Request<M, R>) -> Event<M, R> {
        Message(message)
    }

    fn exec(f: Box<FnOnce<(),()> + Send>) -> Event<M, R> {
        Exec(f)
    }
}

impl<M: Send, R: Async> fmt::Show for Event<M, R> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Message(..) => write!(fmt, "Message"),
            Exec(..) => write!(fmt, "Exec"),
        }
    }
}

impl fmt::Show for SysEvent {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Spawn => write!(fmt, "Spawn"),
            Link(..) => write!(fmt, "Link"),
            Terminate => write!(fmt, "Terminate"),
            ChildTerminated(..) => write!(fmt, "ChildTerminated"),
        }
    }
}
