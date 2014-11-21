use {Actor, ActorRef, actor_ref};
use core::Runtime;
use util::Async;
use std::i64;
use std::time::Duration;

/// Spawns a new actor. Must be called from an actor.
pub fn spawn<M: Send, R: Async, A: Actor<M, R>>(actor: A) -> ActorRef<A, M, R> {
    Runtime::current().spawn(actor, None::<&ActorRef<A, M, R>>)
}

pub struct System {
    runtime: Runtime,
}

impl System {
    pub fn new() -> System {
        System { runtime: Runtime::new() }
    }

    pub fn current() -> System {
        System { runtime: Runtime::current() }
    }

    pub fn start(&self) {
        self.runtime.start();
    }

    pub fn shutdown(&self) {
        self.shutdown_timed(Duration::milliseconds(i64::MAX));
    }

    pub fn shutdown_timed(&self, timeout: Duration) {
        self.runtime.shutdown(timeout);
    }

    pub fn spawn<M: Send, R: Async, A: Actor<M, R>>(&self, actor: A) -> ActorRef<A, M, R> {
        self.start(); // Ensure that the system is running
        self.runtime.spawn(actor, None::<&ActorRef<A, M, R>>)
    }

    pub fn spawn_link<M1: Send, M2: Send, R1: Async, R2: Async, A1: Actor<M1, R1>, A2: Actor<M2, R2>>(&self, actor: A1, supervisor: &ActorRef<A2, M2, R2>) -> ActorRef<A1, M1, R1> {
        self.start(); // Ensure that the system is running
        self.runtime.spawn(actor, Some(supervisor))
    }
}

impl Clone for System {
    fn clone(&self) -> System {
        System { runtime: self.runtime.clone() }
    }
}
