//! Main implementation of the actor system execution.
//!
//! Scheduler abstracts the details of executing an actor system. This allows
//! different implementations for development, test, and production modes.

use {Actor, ActorRef};
use actor;
use core::{Cell, Event, Spawn};
use util::Async;
use std::sync::{Arc};
use std::sync::atomic::{AtomicUint, Relaxed};
use std::time::Duration;

use self::scheduler::Scheduler;
pub use self::scheduler::currently_scheduled;

#[cfg(ndebug)]
mod scheduler;

#[cfg(not(ndebug))]
#[path = "scheduler_dev.rs"]
mod scheduler;

pub trait Schedule {
    // Scheduler tick, returns whether ot not to reschedule for another
    // iteration.
    fn tick(&self) -> bool;

    // Schedule the function to execute in the context of this schedulable type
    fn schedule(&self, f: Box<FnOnce<(),()> + Send>) -> Box<FnOnce<(),()> + Send>;

    fn runtime(&self) -> Runtime;
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
        self.inner.start();
    }

    pub fn shutdown(&self, timeout: Duration) {
        self.inner.shutdown(timeout);
    }

    // Dispatches the event to the specified actor, scheduling it if needed
    pub fn dispatch<Msg: Send, Ret: Async, A: Actor<Msg, Ret>>(&self, cell: Cell<Msg, A>, event: Event<Msg>) {
        self.inner.dispatch(cell, event);
    }

    /// Spawn a new actor
    pub fn spawn<Msg: Send, Ret: Async, A: Actor<Msg, Ret>>(&self, actor: A) -> ActorRef<Msg, A> {
        debug!("spawning actor");
        let cell = Cell::new(actor, self.clone());
        self.inner.dispatch(cell.clone(), Spawn);
        actor::new_ref(cell)
    }
}

impl Clone for Runtime {
    fn clone(&self) -> Runtime {
        Runtime { inner: self.inner.clone() }
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
    fn start(&self) {
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

        self.scheduler.start();
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
    fn dispatch<Msg: Send, Ret: Async, A: Actor<Msg, Ret>>(&self, cell: Cell<Msg, A>, event: Event<Msg>) {
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
