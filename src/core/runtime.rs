//! Main implementation of the actor system execution.
//!
//! Scheduler abstracts the details of executing an actor system. This allows
//! different implementations for development, test, and production modes.

use {Actor, ActorRef};
use actor_ref;
use self::State::*;
use core::{Cell, CellRef, Event, Scheduler, currently_scheduled};
use core::Event::{Spawn};
use sys::{Init, User};
use util::Async;
use std::{mem, ptr};
use std::sync::{Arc, Weak};
use std::sync::atomic::{AtomicUint, Relaxed};
use std::time::Duration;

pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

impl Runtime {
    pub fn new() -> Runtime {
        unsafe {
            let mut rt = Runtime { inner: Arc::new(RuntimeInner::new()) };
            // Yes, you will cry, but it's OK
            // TODO: This is very dangerous given that the init actor may
            // access the runtime
            ptr::write(mem::transmute(&rt.inner.init), Cell::new(Init::new(), rt.weak()));
            rt
        }
    }

    pub fn current() -> Runtime {
        unsafe {
            match currently_scheduled() {
                Some(s) => s.runtime().clone(),
                None => panic!("must be called in context of an actor"),
            }
        }
    }

    pub fn start(&self) {
        self.inner.start();
    }

    pub fn shutdown(&self, timeout: Duration) {
        self.inner.shutdown(timeout);
    }

    // Dispatches the event to the specified actor, scheduling it if needed
    pub fn dispatch<Msg: Send, Ret: Async, A: Actor<Msg, Ret>>(&self, cell: Cell<A, Msg, Ret>, event: Event<Msg, Ret>) {
        self.inner.dispatch(cell, event);
    }

    /// Spawn a new actor
    pub fn spawn<M1: Send, R1: Async, A1: Actor<M1, R1>, M2: Send, R2: Async, A2: Actor<M2, R2>>(&self, actor: A1, supervisor: Option<&ActorRef<A2, M2, R2>>) -> ActorRef<A1, M1, R1> {
        // TODO: Link before spawning
        debug!("spawning actor");
        let cell = Cell::new(actor, self.weak());
        self.inner.dispatch(cell.clone(), Spawn);
        actor_ref::new(cell)
    }

    fn weak(&self) -> RuntimeWeak {
        RuntimeWeak::new(self.inner.downgrade())
    }
}

impl Clone for Runtime {
    fn clone(&self) -> Runtime {
        Runtime { inner: self.inner.clone() }
    }
}

pub struct RuntimeWeak {
    inner: Weak<RuntimeInner>,
}

impl RuntimeWeak {
    fn new(inner: Weak<RuntimeInner>) -> RuntimeWeak {
        RuntimeWeak { inner: inner }
    }

    pub fn upgrade(&self) -> Option<Runtime> {
        self.inner.upgrade()
            .map(|inner| Runtime { inner: inner })
    }
}

/*
 *
 * ===== Implementation =====
 *
 */

struct RuntimeInner {
    state: AtomicUint,
    scheduler: Scheduler,
    init: Cell<Init, (), ()>,
}

impl RuntimeInner {
    unsafe fn new() -> RuntimeInner {
        RuntimeInner {
            state: AtomicUint::new(New as uint),
            scheduler: Scheduler::new(),
            init: mem::zeroed(),
        }
    }

    /// Start the runtime if it has not already been started
    fn start(&self) {
        let mut expect = self.state.load(Relaxed);

        loop {
            let curr: State = FromPrimitive::from_uint(expect)
                .expect("[BUG] invalid state");

            let next = match curr {
                // Transition from New to Running
                New => Running,
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
                New | Running => ShuttingDown,
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
    fn dispatch<Msg: Send, Ret: Async, A: Actor<Msg, Ret>>(&self, cell: Cell<A, Msg, Ret>, event: Event<Msg, Ret>) {
        self.scheduler.dispatch(cell, event);
    }
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        debug!("dropping RuntimeInner");
        self.scheduler.shutdown(Duration::milliseconds(0));
    }
}

struct SysActors {
    init: ActorRef<Init, (), ()>,
    user: ActorRef<User, (), ()>,
}

impl SysActors {
    fn spawn(rt: &Runtime) -> SysActors {
        unimplemented!()
    }
}

#[deriving(Show, FromPrimitive)]
#[repr(uint)]
enum State {
    New,
    Running,
    ShuttingDown,
    Shutdown,
}
