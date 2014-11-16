use {Actor};
use core::{Event, Spawn, Message, Exec, Runtime, RuntimeWeak};
use core::future::Request;
use util::Async;

use std::cell::UnsafeCell;
use std::mem;
use std::num::FromPrimitive;
use std::raw::TraitObject;
use std::sync::Arc;
use std::sync::atomic::{AtomicUint, Relaxed};
use syncbox::{Consume, Produce, LinkedQueue};

pub struct ActorCell<A, M: Send, R: Async> {
    inner: Arc<CellInner<A, M, R>>,
}

// The functions that need to be accessible via trait object
pub trait Cell : Send + Sync {
    // Scheduler tick, returns whether ot not to reschedule for another
    // iteration.
    fn tick(&self) -> bool;

    // Schedule the function to execute in the context of this schedulable type
    fn schedule(&self, f: Box<FnOnce<(),()> + Send>) -> Box<FnOnce<(),()> + Send>;

    // Get the runtime behind this cell
    fn runtime(&self) -> Runtime;
}

struct CellRef {
    cell: TraitObject,
}

impl CellRef {
    fn new(cell: &Cell) -> CellRef {
        CellRef { cell: unsafe { mem::transmute(cell) } }
    }

    fn cell(&self) -> &Cell {
        unsafe { mem::transmute(self.cell) }
    }
}

impl<Msg: Send, Ret: Async, A: Actor<Msg, Ret>> ActorCell<A, Msg, Ret> {
    pub fn new(actor: A, runtime: RuntimeWeak) -> ActorCell<A, Msg, Ret> {
        ActorCell {
            inner: Arc::new(CellInner::new(actor, runtime))
        }
    }

    pub fn send_request(&self, request: Request<Msg, Ret>) {
        self.inner.send_request(request, self.clone());
    }

    pub fn deliver_event(&self, event: Event<Msg, Ret>) -> bool {
        self.inner.deliver_event(event)
    }
}

impl<Msg: Send, Ret: Async, A: Actor<Msg, Ret>> Cell for ActorCell<A, Msg, Ret> {
    fn tick(&self) -> bool {
        self.inner.tick()
    }

    fn schedule(&self, f: Box<FnOnce<(),()> + Send>) -> Box<FnOnce<(),()> + Send> {
        let cell = self.clone();

        box move |:| { cell.inner.deliver_event(Event::exec(f)); }
    }

    fn runtime(&self) -> Runtime {
        self.inner.runtime().clone()
    }
}

impl<Msg: Send, Ret: Async, A: Actor<Msg, Ret>> Clone for ActorCell<A, Msg, Ret> {
    fn clone(&self) -> ActorCell<A, Msg, Ret> {
        ActorCell { inner: self.inner.clone() }
    }
}

// TODO:
// - Use improved mailbox queue. This can be a MPSC queue w/ no waiting since
//   coordination happens externally.
// - Consider whether or not to split it out into a handle and core for better
//   mut safety.
// - The cell needs to know which runtime it is from, but using std::Arc is
//   pretty heavy. Migrate to a version that keeps thread local ref counts.
struct CellInner<A, M: Send, R: Async> {
    // The actor that the cell is powering
    actor: UnsafeCell<A>,
    // The state of the actor
    state: AtomicUint,
    // The runtime that the actor belongs to
    runtime: RuntimeWeak,
    // The mailbox for all user level messages
    mailbox: LinkedQueue<Event<M, R>>,
    // The mailbox for all system messages
    sys_mailbox: LinkedQueue<Event<M, R>>,
    // The supervisor
    supervisor: Option<CellRef>,
    // Supervised actors - TODO: Not the right types
    children: Vec<Box<Cell+Send+Sync>>,
}

impl<Msg: Send, Ret: Async, A: Actor<Msg, Ret>> CellInner<A, Msg, Ret> {
    fn new(actor: A, runtime: RuntimeWeak) -> CellInner<A, Msg, Ret> {
        CellInner {
            actor: UnsafeCell::new(actor),
            state: AtomicUint::new(Init as uint),
            runtime: runtime,
            mailbox: LinkedQueue::new(),
            sys_mailbox: LinkedQueue::new(),
            supervisor: None,
            children: vec![],
        }
    }

    pub fn send_request(&self, request: Request<Msg, Ret>, cell: ActorCell<A, Msg, Ret>) {
        debug!("sending message");
        self.runtime().dispatch(cell, Event::message(request));
    }

    // Atomically enqueues the message and returns whether or not the actor
    // should be scheduled.
    fn deliver_event(&self, event: Event<Msg, Ret>) -> bool {
        // Track if the event is a spawn
        let spawn = event.is_spawn();

        // First enqueue the message:
        self.enqueue_event(event);

        let mut expect = self.state.load(Relaxed);

        loop {
            let curr: State = FromPrimitive::from_uint(expect)
                .expect("[BUG] invalid state");

            debug!("deliver_event; state={}; is_spawn={}", curr, spawn);

            let next = match curr {
                // Cases in which there is never a need to schedule
                Scheduled | Pending | Shutdown | Crashed => return false,
                // When in the initial state, only request a schedule when the event is a spawn
                Init => return spawn,
                // If the actor is currently idle, it needs to be scheduled to
                // process the new message. Transition to a Scheduled state.
                Idle => Scheduled,
                // If the actor is currently running, there is no need to
                // schedule it, but it needs to be signaled that there are new
                // messages to process. Transition to the Pending state.
                Running => Pending,
            };

            let actual = self.state.compare_and_swap(expect, next as uint, Relaxed);

            if actual == expect {
                match next {
                    Scheduled => {
                        debug!("scheduling actor");
                        return true;
                    }
                    _ => return false, // Only other option is Pending
                }
            }

            // CAS failed, try again
            expect = actual;
        }
    }

    fn enqueue_event(&self, event: Event<Msg, Ret>) {
        // TODO: Handle error
        if event.is_message() {
            self.mailbox.put(event).ok().unwrap();
        } else {
            self.sys_mailbox.put(event).ok().unwrap();
        }
    }

    // Execute a single iteration of the actor
    fn tick(&self) -> bool {
        debug!("ActorCell::tick");

        // Transition the cell to the running state
        let mut expect = self.state.load(Relaxed);

        loop {
            let curr: State = FromPrimitive::from_uint(expect)
                .expect("[BUG] invalid state");

            let next = match curr {
                // If currently running, then nothing to do
                Running | Shutdown | Crashed => return false,
                Init | Scheduled => Running,
                Pending | Idle => panic!("unexpected state {}", curr),
            };

            let actual = self.state.compare_and_swap(expect, next as uint, Relaxed);

            if actual == expect {
                break;
            }

            // CAS failed, try again
            expect = actual;
        }

        let reschedule = self.process_queue();

        // Transition to next state
        let mut expect = self.state.load(Relaxed);

        loop {
            let curr: State = FromPrimitive::from_uint(expect)
                .expect("[BUG] invalid state");

            let next = match curr {
                Pending => Scheduled,
                Running => if reschedule { Scheduled } else { Idle },
                Shutdown | Crashed => return false,
                Init | Scheduled | Idle => panic!("unexpected state {}", curr),
            };

            let actual = self.state.compare_and_swap(expect, next as uint, Relaxed);

            if actual == expect {
                return next == Scheduled;
            }

            // CAS failed, try again
            expect = actual
        }
    }

    fn process_queue(&self) -> bool {
        debug!("ActorCell::process_queue");

        // First process all system events
        while let Some(event) = self.sys_mailbox.take() {
            self.process_event(event);
        }

        // Process a single user event
        if let Some(event) = self.mailbox.take() {
            self.process_event(event);
            return true;
        }

        debug!("  no messages to process");
        return false;
    }

    fn process_event(&self, event: Event<Msg, Ret>) {
        debug!("processing event; event={}", event);

        match event {
            Message(msg) => self.receive_msg(msg),
            Exec(f) => f.call_once(()),
            Spawn => self.actor().prepare(),
        }
    }

    fn receive_msg(&self, request: Request<Msg, Ret>) {
        let Request { message, response } = request;
        self.actor().receive(message).link(response);
    }

    fn runtime(&self) -> Runtime {
        self.runtime.upgrade()
            .expect("[BUG] runtime has been finalized").clone()
    }

    // Pretty hacky
    fn actor<'a>(&'a self) -> &'a mut A {
        use std::mem;

        unsafe {
            let actor: &mut A = mem::transmute(self.actor.get());
            actor
        }
    }
}

#[deriving(Show, FromPrimitive, PartialEq)]
enum State {
    Init,      // Initial actor state
    Idle,      // Spawned, no pending messages
    Scheduled, // Spawned, scheduled for execution (pending messages)
    Running,   // Spawned, currently processing messages
    Pending,   // Spawned, currently processing messages, more enqueued
    Shutdown,  // Successfuly finished execution
    Crashed,   // Terminated with failure
}

impl State {
}