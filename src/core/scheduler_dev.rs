use {Actor};
use self::Op::*;
use self::State::*;
use core::{Cell, CellRef, Event, SysEvent};
use util::Async;
use syncbox::{LinkedQueue, Consume, Produce};
use syncbox::locks::{MutexCell, CondVar};
use std::mem;
use std::sync::Arc;
use std::time::Duration;

// TODO:
// - Use a single threaded executor from syncbox
// - Use a future to track worker shutdown
// - Switch to unboxed closures:
//      https://github.com/rust-lang/rust/issues/18378
//

pub struct Scheduler {
    inner: Arc<SchedulerInner>,
}

pub struct SchedulerInner {
    queue: LinkedQueue<Op>,
    state: MutexCell<State>,
    condvar: CondVar,
}

#[thread_local]
static mut SCHEDULED: Option<&'static CellRef> = None;

pub unsafe fn currently_scheduled<'a>() -> Option<&'a CellRef> {
    mem::transmute(SCHEDULED)
}

impl Scheduler {
    pub fn new() -> Scheduler {
        Scheduler {
            inner: Arc::new(SchedulerInner {
                queue: LinkedQueue::new(),
                state: MutexCell::new(Running),
                condvar: CondVar::new(),
            }),
        }
    }

    pub fn start(&self) {
        let inner = self.inner.clone();
        spawn(proc() worker_loop(inner));
    }

    pub fn shutdown(&self, timeout: Duration) {
        self.inner.shutdown(timeout);
    }

    // Dispatches the event to the specified actor, scheduling it if needed
    pub fn dispatch<M: Send, R: Async, A: Actor<M, R>>(&self, cell: Cell<A, M, R>, event: Event<M, R>) {
        debug!("dispatching event to cell");

        // TODO: Handle dispatching to shutdown / crashed actor
        if cell.deliver_event(event) {
            debug!("  cell requires scheduling");
            self.inner.schedule_cell(cell.to_ref());
        }
    }

    // Dispatches the event to the specified actor, scheduling it if needed
    pub fn sys_dispatch(&self, cell: CellRef, event: SysEvent) {
        debug!("dispatching sys event to cell");

        // TODO: Handle dispatching to shutdown / crashed actor
        if cell.deliver_sys_event(event) {
            debug!("  cell requires scheduling");
            self.inner.schedule_cell(cell);
        }
    }
}

impl SchedulerInner {
    fn shutdown(&self, timeout: Duration) {
        let mut lock = self.state.lock();

        if *lock == Running {
            *lock = Terminating;
            self.enqueue(Terminate);
        }

        if *lock == Terminating {
            // Technically, this would need to be called in a loop until
            // the state transitioned to Terminated or the timeout fully
            // ran out.
            lock.timed_wait(&self.condvar, timeout.num_milliseconds() as uint);
        }
    }

    fn schedule_cell(&self, cell: CellRef) {
        self.enqueue(Task(cell));
    }

    fn enqueue(&self, op: Op) {
        self.queue.put(op)
            .ok().expect("[BUG] cell scheduling failure not implemented");
    }
}

enum Op {
    Task(CellRef),
    Terminate,
}

// ===== Background worker =====

fn worker_loop(scheduler: Arc<SchedulerInner>) {
    use std::time::Duration;

    loop {
        debug!("queue wait");
        if let Some(op) = scheduler.queue.take_wait(Duration::seconds(120)) {

            match op {
                Task(mut scheduled) => {
                    unsafe { SCHEDULED = Some(mem::transmute(&scheduled)) };

                    // If true, requires reschedule
                    if scheduled.tick() {
                        debug!("more work to be done, rescheduling");
                        scheduler.queue.put(Task(scheduled))
                            .ok().expect("[BUG] not handled");
                    }

                    unsafe { SCHEDULED = None; }
                }
                Terminate => break,
            }
        }
    }

    debug!("terminating worker thread");

    let mut lock = scheduler.state.lock();

    // Transition to the terminated state
    *lock = Terminated;

    // Signal any waiting threads
    scheduler.condvar.signal();
}

#[deriving(PartialEq)]
enum State {
    Running,
    Terminating,
    Terminated,
}
