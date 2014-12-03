//! Main implementation of the actor system execution.
//!
//! Scheduler abstracts the details of executing an actor system. This allows
//! different implementations for development, test, and production modes.

use {Actor, ActorRef};
use actor_ref;
use self::State::*;
use core::{Cell, Event, SysEvent, Scheduler, currently_scheduled};
use core::SysEvent::{Spawn};
use sys::{Init, User};
use util::Async;
use std::mem;
use std::sync::{Arc, Weak};
use std::sync::atomic::{AtomicUint, Relaxed};
use std::time::Duration;

pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

impl Runtime {
    pub fn new() -> Runtime {
        let rt = Runtime { inner: Arc::new(RuntimeInner::new()) };
        let sys = SysActors::spawn(&rt);

        unsafe {
            mem::replace(mem::transmute(&rt.inner.sys_actors), Some(sys));
        }

        rt
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
        self.inner.spawn(Cell::new(actor, self.weak()), supervisor)
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
    sys_actors: Option<SysActors>,
}

impl RuntimeInner {
    fn new() -> RuntimeInner {
        RuntimeInner {
            state: AtomicUint::new(New as uint),
            scheduler: Scheduler::new(),
            sys_actors: None,
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

    /// Spawn a new actor
    pub fn spawn<M1: Send, R1: Async, A1: Actor<M1, R1>, M2: Send, R2: Async, A2: Actor<M2, R2>>(&self, cell: Cell<A1, M1, R1>, supervisor: Option<&ActorRef<A2, M2, R2>>) -> ActorRef<A1, M1, R1> {
        debug!("spawning actor");

        match supervisor {
            Some(supervisor) => self.scheduler.spawn_link(cell.to_ref(), actor_ref::to_ref(supervisor)),
            _ => {
                // No specified supervisor, use the current actor
                match unsafe { currently_scheduled() } {
                    Some(curr) => self.scheduler.spawn_link(cell.to_ref(), curr.clone()),
                    _ => {
                        // No actor currently running, run under user actor
                        match self.sys_actors {
                            Some(ref sys) => self.scheduler.spawn_link(cell.to_ref(), actor_ref::to_ref(&sys.user)),
                            _ => {
                                // Currently booting up, this must be Init,
                                // simply spawn
                                self.sys_dispatch(cell.clone(), Spawn);
                            }
                        }

                    }
                }
            }
        }

        actor_ref::new(cell)
    }

    // Dispatches the event to the specified actor, scheduling it if needed
    fn dispatch<M: Send, R: Async, A: Actor<M, R>>(&self, cell: Cell<A, M, R>, event: Event<M, R>) {
        self.scheduler.dispatch(cell, event);
    }

    fn sys_dispatch<M: Send, R: Async, A: Actor<M, R>>(&self, cell: Cell<A, M, R>, event: SysEvent) {
        self.scheduler.sys_dispatch(cell, event);
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
        let init = rt.spawn(Init::new(), None::<&ActorRef<Init, (), ()>>);
        let user = rt.spawn(User::new(), Some(&init));

        SysActors { init: init, user: user }
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
