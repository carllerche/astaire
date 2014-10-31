
pub trait Future : Send {
    // Invoke the callback with the future on completion
    fn ready<F: FnOnce(Self)>(self, cb: F);
}

pub struct Val<T>;

impl<T: Send> Future for Val<T> {
    fn ready<F: FnOnce(Val<T>)>(self, _cb: F) {
        unimplemented!();
    }
}

pub struct Stream<T>;

impl<T: Send> Future for Stream<T> {
    fn ready<F: FnOnce(Stream<T>)>(self, _cb: F) {
        unimplemented!();
    }
}

impl Future for () {
    fn ready<F: FnOnce(())>(self, cb: F) {
        cb(self);
    }
}
