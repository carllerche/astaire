use super::Future;

pub struct Stream<T>;

impl<T: Send> Future for Stream<T> {
    fn ready<F: FnOnce(Stream<T>)>(self, _cb: F) {
        unimplemented!();
    }
}
