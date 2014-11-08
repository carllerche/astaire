// pub use self::stream::Stream;
pub use self::val::{Val, Producer};

// mod stream;
mod val;

pub trait Future : Send {
    // Invoke the callback with the future on completion
    fn ready<F: Send + FnOnce(Self)>(self, cb: F);
}

impl Future for () {
    fn ready<F: Send + FnOnce(())>(self, cb: F) {
        cb(self);
    }
}
