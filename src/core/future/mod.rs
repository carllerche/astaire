// pub use self::stream::Stream;
pub use self::future::{Future, Completer};

// mod stream;
mod future;

pub trait Async : Send {
    // Invoke the callback with the future on completion
    fn ready<F: Send + FnOnce(Self)>(self, cb: F);
}

impl Async for () {
    fn ready<F: Send + FnOnce(())>(self, cb: F) {
        cb(self);
    }
}
