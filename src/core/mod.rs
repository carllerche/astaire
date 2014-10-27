pub use self::cell::Cell;
pub use self::rt::{Runtime, RuntimeWeak};
use std::fmt;

mod cell;
pub mod rt;

pub trait Coordinator {
    fn dispatch(/*recipient: Cell*/);
}

enum Event<M> {
    Message(M),
    Spawn,
}

impl<T> Event<T> {
    fn message(message: T) -> Event<T> {
        Message(message)
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
            Spawn => write!(fmt, "Spawn"),
        }
    }
}
