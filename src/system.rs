use {Actor, ActorRef};
use core::{Runtime};
use util::Async;
use std::i64;
use std::time::Duration;

/// Spawns a new actor. Must be called from an actor.
pub fn spawn<M: Send, A: Actor<M>>(actor: A) -> ActorRef<M, A> {
    Runtime::current().spawn(actor)
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

    pub fn spawn<M: Send, Ret: Async, A: Actor<M, Ret>>(&self, actor: A) -> ActorRef<M, A> {
        self.start(); // Ensure that the system is running
        self.runtime.spawn(actor)
    }
}

impl Clone for System {
    fn clone(&self) -> System {
        System { runtime: self.runtime.clone() }
    }
}
