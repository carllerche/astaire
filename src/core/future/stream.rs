use super::Async;

pub struct Stream<T>;

impl<T: Send> Async for Stream<T> {
    fn ready<F: FnOnce(Stream<T>)>(self, _cb: F) {
        unimplemented!();
    }
}
