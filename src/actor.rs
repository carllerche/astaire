use core::{Cell};
use util::Future;

pub trait Actor<Msg: Send, Ret: Future = ()> : Send {
    fn prepare(&mut self) {
    }

    fn receive(&mut self, msg: Msg) -> Ret;
}

/*
 * TODO: Specifying the return produces ICE
 */
impl<Msg: Send, R: Future, F: Send + FnMut(Msg) -> R> Actor<Msg, R> for F {
    fn receive(&mut self, msg: Msg) -> R {
        self.call_mut((msg,))
    }
}

// There are multiple kinds of actor refs:
// - Refs that are useable outside of the system
// - Local refs pointing to regular actors
// - Inlined refs (no message dispatching)
// - Remote refs pointing to actors in other systems
pub struct ActorRef<M, A> {
    cell: Cell<M, A>,
}

impl<M: Send, A: Actor<M>> ActorRef<M, A> {
    /// Sends a message to the specified actor
    pub fn send(&self, msg: M) {
        self.cell.send_message(msg);
    }
}

// Separate fn to keep the ActorRef public API clean
pub fn new_ref<M, A>(cell: Cell<M, A>) -> ActorRef<M, A> {
    ActorRef { cell: cell }
}