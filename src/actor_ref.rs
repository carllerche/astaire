use {Actor};
use core::{Cell, CellRef};
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
        self.cell.send(msg)
    }
}

impl<M: Send, R: Async, A: Actor<M, R>> Clone for ActorRef<A, M, R> {
    fn clone(&self) -> ActorRef<A, M, R> {
        ActorRef { cell: self.cell.clone() }
    }
}

/*
 * === Separated out to keep ActorRef public API clean ===
 */

pub fn new<M: Send, R: Async, A: Actor<M, R>>(cell: Cell<A, M, R>) -> ActorRef<A, M, R> {
    ActorRef { cell: cell }
}

pub fn to_ref<M: Send, R: Async, A: Actor<M, R>>(actor_ref: &ActorRef<A, M, R>) -> CellRef {
    actor_ref.cell.to_ref()
}

/*
// Returns the CellPtr for the ref and increments the ref-count
pub fn ptr_retained<M: Send, R: Async, A: Actor<M, R>>(actor_ref: &ActorRef<A, M, R>) -> CellPtr {
    unimplemented!()
}
*/
