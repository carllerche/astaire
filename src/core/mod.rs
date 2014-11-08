pub use self::cell::Cell;
pub use self::rt::{Runtime};
use std::fmt;

mod cell;
pub mod future;
pub mod rt;

pub trait Coordinator {
    fn dispatch(/*recipient: Cell*/);
}

enum Event<M> {
    Message(M),
    Spawn,
    Exec(Box<FnOnce<(),()> + Send>),
}

impl<T> Event<T> {
    fn message(message: T) -> Event<T> {
        Message(message)
    }

    fn exec(f: Box<FnOnce<(),()> + Send>) -> Event<T> {
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

impl<T> fmt::Show for Event<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Message(..) => write!(fmt, "Message"),
            Exec(..) => write!(fmt, "Exec"),
            Spawn => write!(fmt, "Spawn"),
        }
    }
}
