// pub use self::stream::Stream;
pub use self::future::{Future, Completer};

// mod stream;
mod future;

pub trait Async : Send {
    // Invoke the callback with the future on completion
    fn ready<F: Send + FnOnce(Self)>(self, cb: F);

    /*
     *
     * ====== Internal API ======
     *
     */

    // Returns an async response
    fn request<Msg>(msg: Msg) -> (Request<Msg, Self>, Self);

    fn link(self, resp: Response<Self>);
}

impl Async for () {
    fn ready<F: Send + FnOnce(())>(self, cb: F) {
        cb(self);
    }

    fn request<Msg>(msg: Msg) -> (Request<Msg, ()>, ()) {
        (Request::new(msg, ()), ())
    }

    fn link(self, _: Response<()>) {
    }
}

pub struct Request<Msg, T: Async> {
    pub message: Msg,
    pub response: Response<T>,
}

impl<Msg, T: Async> Request<Msg, T> {
    fn new(message: Msg, async: T) -> Request<Msg, T> {
        Request {
            message: message,
            response: Response {
                async: async,
            }
        }
    }
}

pub struct Response<T: Async> {
    async: T,
}
