use {Actor};
use core::{Cell, Event, Runtime, RuntimeWeak};
use core::rt::Tick;
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

// Gets a pointer to the current system
#[thread_local]
static mut RUNTIME: *const Runtime = 0 as *const Runtime;

pub fn current_runtime<'a>() -> &'a Runtime {
    unsafe {
        if RUNTIME.is_null() {
            panic!("must be called in context of an actor");
        }

        mem::transmute(RUNTIME)
    }
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

    pub fn start(&self, runtime: RuntimeWeak) {
        let inner = self.inner.clone();

        spawn(proc() worker_loop(inner, runtime));
    }

    pub fn shutdown(&self, timeout: Duration) {
        self.inner.shutdown(timeout);
    }

    // Dispatches the event to the specified actor, scheduling it if needed
    pub fn dispatch<M: Send, A: Actor<M>>(&self, cell: Cell<M, A>, event: Event<M>) {
        debug!("dispatching event to cell");
        if cell.deliver_event(event) {
            debug!("  cell requires scheduling");
            self.inner.schedule_actor(cell);
        }
    }
}

impl SchedulerInner {
    fn shutdown(&self, timeout: Duration) {
        let mut lock = self.state.lock();

        if *lock == Running {
            debug!("enqueuing terimate token");
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

    // Schedule the actor for execution[
    fn schedule_actor<M: Send, A: Actor<M>>(&self, cell: Cell<M, A>) {
        // self.enqueue(Task(proc() -> bool { cell.tick() }));
        self.enqueue(Task(box cell as Box<Tick + Send>));
    }

    fn enqueue(&self, op: Op) {
        self.queue.put(op)
            .ok().expect("[BUG] cell scheduling failure not implemented");
    }
}

enum Op {
    Task(Box<Tick + Send>),
    Terminate,
}

// ===== Background worker =====

fn worker_loop(scheduler: Arc<SchedulerInner>, runtime: RuntimeWeak) {
    use std::time::Duration;

    loop {
        if let Some(op) = scheduler.queue.take_wait(Duration::seconds(120)) {

            match op {
                Task(t) => {
                    if let Some(rt) = runtime.upgrade() {
                        unsafe { RUNTIME = &rt as *const Runtime };

                        // If true, requires reschedule
                        if t.tick() {
                            debug!("more work to be done, rescheduling");
                            scheduler.queue.put(Task(t))
                                .ok().expect("[BUG] not handled");
                        }
                    } else {
                        // The system has been shutdown, exit worker
                        break;
                    }
                }
                Terminate => break,
            }
        }
    }

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
