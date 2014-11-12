use util::Async;

pub trait Actor<Msg: Send, Ret: Async = ()> : Send {
    fn prepare(&mut self) {
    }

    fn receive(&mut self, msg: Msg) -> Ret;
}

impl<Msg: Send, R: Async, F: Send + FnMut(Msg) -> R> Actor<Msg, R> for F {
    fn receive(&mut self, msg: Msg) -> R {
        self.call_mut((msg,))
    }
}
