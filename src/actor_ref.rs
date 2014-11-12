use {Actor};
use core::Cell;
use core::future::Request;
use util::{Async};

// There are multiple kinds of actor refs:
// - Refs that are useable outside of the system
// - Local refs pointing to regular actors
// - Inlined refs (no message dispatching)
// - Remote refs pointing to actors in other systems
pub struct ActorRef<A, M: Send, R: Async> {
    cell: Cell<A, M, R>,
}

impl<M: Send, R: Async, A: Actor<M, R>> ActorRef<A, M, R> {
    /// Sends a message to the specified actor
    pub fn send(&self, msg: M) -> R {
        let (request, response): (Request<M, R>, R) = Async::request(msg);
        self.cell.send_request(request);
        response
    }
}

// Separate fn to keep the ActorRef public API clean
pub fn new<M: Send, R: Async, A: Actor<M, R>>(cell: Cell<A, M, R>) -> ActorRef<A, M, R> {
    ActorRef { cell: cell }
}
