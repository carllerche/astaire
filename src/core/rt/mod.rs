//! Main implementation of the actor system execution.
//!
//! Scheduler abstracts the details of executing an actor system. This allows
//! different implementations for development, test, and production modes.

use {Actor, ActorRef};
use actor;
use core::{Cell, Event, Spawn};
use std::sync::{Arc, Weak};
use std::sync::atomic::{AtomicUint, Relaxed};
use std::time::Duration;

use self::scheduler::Scheduler;

#[cfg(ndebug)]
mod scheduler;

#[cfg(not(ndebug))]
#[path = "scheduler_dev.rs"]
mod scheduler;

pub trait Tick {
    // Scheduler tick, returns whether ot not to reschedule for another
    // iteration.
    fn tick(&self) -> bool;
}

/*
/// Gets the system for the current thread
pub fn current_system<'a>() -> &'a System {
    scheduler::current_system()
}
*/

pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

impl Runtime {
    pub fn new() -> Runtime {
        Runtime {
            inner: Arc::new(RuntimeInner::new()),
        }
    }

    pub fn current() -> Runtime {
        scheduler::current_runtime().clone()
    }

    pub fn start(&self) {
        self.inner.start(self.downgrade());
    }

    pub fn shutdown(&self, timeout: Duration) {
        self.inner.shutdown(timeout);
    }

    // Dispatches the event to the specified actor, scheduling it if needed
    pub fn dispatch<M: Send, A: Actor<M>>(&self, cell: Cell<M, A>, event: Event<M>) {
        self.inner.dispatch(cell, event);
    }

    /// Spawn a new actor
    pub fn spawn<M: Send, A: Actor<M>>(&self, actor: A) -> ActorRef<M, A> {
        debug!("spawning actor");
        let cell = Cell::new(actor, self.clone());
        self.inner.dispatch(cell.clone(), Spawn);
        actor::new_ref(cell)
    }

    pub fn downgrade(&self) -> RuntimeWeak {
        RuntimeWeak { inner: self.inner.downgrade() }
    }

    // The following fns should be called in context of an actor
}

impl Clone for Runtime {
    fn clone(&self) -> Runtime {
        Runtime { inner: self.inner.clone() }
    }
}

// Weak ref to the runtime
pub struct RuntimeWeak {
    inner: Weak<RuntimeInner>,
}

impl RuntimeWeak {
    pub fn upgrade(&self) -> Option<Runtime> {
        self.inner.upgrade()
            .map(|rt| Runtime { inner: rt })
    }
}

struct RuntimeInner {
    state: AtomicUint,
    scheduler: scheduler::Scheduler,
}

impl RuntimeInner {
    fn new() -> RuntimeInner {
        RuntimeInner {
            state: AtomicUint::new(Init as uint),
            scheduler: Scheduler::new(),
        }
    }

    /// Start the runtime if it has not already been started
    fn start(&self, runtime: RuntimeWeak) {
        let mut expect = self.state.load(Relaxed);

        loop {
            let curr: State = FromPrimitive::from_uint(expect)
                .expect("[BUG] invalid state");

            let next = match curr {
                // Transition from Init to Running
                Init => Running,
                // Nothing to do
                _ => return,
            };

            let actual = self.state.compare_and_swap(expect, next as uint, Relaxed);

            if expect == actual {
                break;
            }

            expect = actual;
        }

        self.scheduler.start(runtime);
    }

    /// Shutdown the runtime waiting up to specified time
    fn shutdown(&self, timeout: Duration) {
        let mut expect = self.state.load(Relaxed);

        loop {
            let curr: State = FromPrimitive::from_uint(expect)
                .expect("[BUG] invalid state");

            let next = match curr {
                Init | Running => ShuttingDown,
                ShuttingDown => break,
                Shutdown => return,
            };

            let actual = self.state.compare_and_swap(expect, next as uint, Relaxed);

            if expect == actual {
                break;
            }

            expect = actual;
        }

        // Wait until shutdown
        self.scheduler.shutdown(timeout);

        // Update the state
        self.state.swap(Shutdown as uint, Relaxed);
    }

    // Dispatches the event to the specified actor, scheduling it if needed
    fn dispatch<M: Send, A: Actor<M>>(&self, cell: Cell<M, A>, event: Event<M>) {
        self.scheduler.dispatch(cell, event);
    }
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        self.scheduler.shutdown(Duration::milliseconds(0));
    }
}

#[deriving(Show, FromPrimitive)]
#[repr(uint)]
enum State {
    Init,
    Running,
    ShuttingDown,
    Shutdown,
}
