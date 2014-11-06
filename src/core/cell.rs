use {Actor};
use core::{rt, Event, Spawn, Message, Exec, Runtime};
use util::Future;

use std::cell::UnsafeCell;
use std::mem;
use std::num::FromPrimitive;
use std::sync::Arc;
use std::sync::atomic::{AtomicUint, Relaxed};
use syncbox::{Consume, Produce, LinkedQueue};

pub struct Cell<Msg, A> {
    inner: Arc<CellInner<Msg, A>>,
}

impl<Msg: Send, Ret: Future, A: Actor<Msg, Ret>> Cell<Msg, A> {
    pub fn new(actor: A, runtime: Runtime) -> Cell<Msg, A> {
        Cell {
            inner: Arc::new(CellInner::new(actor, runtime))
        }
    }

    pub fn send_message(&self, msg: Msg) {
        self.inner.send_message(msg, self.clone());
    }

    pub fn deliver_event(&self, event: Event<Msg>) -> bool {
        self.inner.deliver_event(event)
    }
}

impl<Msg: Send, Ret: Future, A: Actor<Msg, Ret>> rt::Schedule for Cell<Msg, A> {
    fn tick(&self) -> bool {
        self.inner.tick()
    }

    fn schedule(&self, f: Box<FnOnce<(),()> + Send>) -> Box<FnOnce<(),()> + Send> {
        let cell = self.clone();

        box move |:| { cell.inner.deliver_event(Event::exec(f)); }
    }

    fn runtime(&self) -> Runtime {
        self.inner.runtime.clone()
    }
}

impl<Msg: Send, Ret: Future, A: Actor<Msg, Ret>> Clone for Cell<Msg, A> {
    fn clone(&self) -> Cell<Msg, A> {
        Cell { inner: self.inner.clone() }
    }
}

// TODO:
// - Use improved mailbox queue. This can be a MPSC queue w/ no waiting since
//   coordination happens externally.
// - Consider whether or not to split it out into a handle and core for better
//   mut safety.
// - The cell needs to know which runtime it is from, but using std::Arc is
//   pretty heavy. Migrate to a version that keeps thread local ref counts.
struct CellInner<Msg, A> {
    // The actor that the cell is powering
    actor: UnsafeCell<A>,
    // The state of the actor
    state: AtomicUint,
    // The runtime that the actor belongs to
    runtime: Runtime,
    // The mailbox for all user level messages
    mailbox: LinkedQueue<Event<Msg>>,
    // THe mailbox for all system messages
    sys_mailbox: LinkedQueue<Event<Msg>>,
}

impl<Msg: Send, Ret: Future, A: Actor<Msg, Ret>> CellInner<Msg, A> {
    fn new(actor: A, runtime: Runtime) -> CellInner<Msg, A> {
        CellInner {
            actor: UnsafeCell::new(actor),
            state: AtomicUint::new(Init as uint),
            runtime: runtime,
            mailbox: LinkedQueue::new(),
            sys_mailbox: LinkedQueue::new(),
        }
    }

    pub fn send_message(&self, msg: Msg, cell: Cell<Msg, A>) {
        debug!("sending message");
        self.runtime.dispatch(cell, Event::message(msg));
    }

    // Atomically enqueues the message and returns whether or not the actor
    // should be scheduled.
    fn deliver_event(&self, event: Event<Msg>) -> bool {
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

    fn enqueue_event(&self, event: Event<Msg>) {
        // TODO: Handle error
        if event.is_message() {
            self.mailbox.put(event).ok().unwrap();
        } else {
            self.sys_mailbox.put(event).ok().unwrap();
        }
    }

    // Execute a single iteration of the actor
    fn tick(&self) -> bool {
        debug!("Cell::tick");

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
        debug!("Cell::process_queue");

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

    fn process_event(&self, event: Event<Msg>) {
        debug!("processing event; event={}", event);

        match event {
            Message(msg) => self.receive_msg(msg),
            Exec(f) => f.call_once(()),
            Spawn => self.actor().prepare(),
        }
    }

    fn receive_msg(&self, msg: Msg) {
        self.actor().receive(msg);
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
