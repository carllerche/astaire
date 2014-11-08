#![allow(dead_code)]

use super::Async;
use core::rt::{Schedule, currently_scheduled};

use std::mem;
use std::sync::Arc;
use syncbox::locks::{MutexCell, MutexCellGuard};

pub struct Future<T> {
    inner: Option<FutureInner<T>>,
}

impl<T: Send> Future<T> {
    // Returns a new future with its producer
    pub fn pair() -> (Future<T>, Completer<T>) {
        let inner = FutureInner::new();

        let val = Future { inner: Some(inner.clone()) };
        let producer = Completer { inner: Some(inner) };

        (val, producer)
    }

    pub fn unwrap(mut self) -> T {
        self.inner.take().unwrap().consumer_unwrap()
    }

    /// Maps the future to Future<U> by applying the provided function
    pub fn map<U: Send, F: Send + FnOnce(T) -> U>(self, op: F) -> Future<U> {
        let (ret, p) = Future::pair();
        self.ready(move |:val: Future<T>| p.put(op(val.unwrap())));
        ret
    }
}

impl<T: Send> Async for Future<T> {
    fn ready<F: Send + FnOnce(Future<T>)>(mut self, cb: F) {
        self.inner.take().unwrap().ready(cb);
    }
}

pub struct Completer<T> {
    inner: Option<FutureInner<T>>,
}

impl<T: Send> Completer<T> {
    pub fn put(mut self, val: T) {
        self.inner.take().unwrap().put(val);
    }

    pub fn fail(self, _desc: &'static str) {
        unimplemented!();
    }
}

// == Implementation details ==
//
// Currently not tuned for performance and implemented with a mutex. Once the
// API stabalizes, this will be rewritten using a lock-free strategy.
struct FutureInner<T> {
    core: Arc<MutexCell<Core<T>>>,
}

impl<T: Send> FutureInner<T> {
    fn new() -> FutureInner<T> {
        FutureInner {
            core: Arc::new(MutexCell::new(Core::new())),
        }
    }

    fn consumer_unwrap(&self) -> T {
        let mut core = self.lock();

        if let Some(val) = core.val.take() {
            return val;
        }

        panic!("future not realized");
    }

    fn ready<F: Send + FnOnce(Future<T>)>(&self, cb: F) {
        // Acquire the lock
        let mut core = self.lock();

        // TODO: Notify the producer

        if core.state.is_ready() {
            drop(core);
            cb(self.val());
            return;
        }

        let inner = self.clone();

        core.push_consumer_wait(Waiter::new(move |:| {
            cb(Future { inner: Some(inner) });
        }));
    }

    fn put(&self, val: T) {
        // Acquire the lock
        let mut core = self.lock();

        // Put the value
        core.put(val);

        // Check if the consumer is waiting on the value, if so, it will
        // be notified that value is ready.
        if let ConsumerWait(waiters) = core.take_consumer_wait() {
            for waiter in waiters.into_iter() {
                // Make sure that the waiter has not been canceled
                if let Some(waiter) = waiter {
                    // Release the lock
                    drop(core);
                    // Invoke the waiter
                    waiter.invoke();
                    // Reacquire the lock
                    core = self.lock();
                }
            }

            return;
        }
    }

    #[inline]
    fn val(&self) -> Future<T> {
        Future { inner: Some(self.clone()) }
    }

    #[inline]
    fn lock(&self) -> MutexCellGuard<Core<T>> {
        self.core.lock()
    }
}

impl<T: Send> Clone for FutureInner<T> {
    fn clone(&self) -> FutureInner<T> {
        FutureInner { core: self.core.clone() }
    }
}

struct Core<T> {
    val: Option<T>,
    state: State<T>,
}

impl<T: Send> Core<T> {
    fn new() -> Core<T> {
        Core {
            val: None,
            state: Pending,
        }
    }

    fn put(&mut self, val: T) {
        assert!(self.val.is_none(), "future already completed");
        self.val = Some(val);
    }

    fn take_consumer_wait(&mut self) -> State<T> {
        if self.state.is_consumer_wait() {
            mem::replace(&mut self.state, Complete)
        } else {
            Complete
        }
    }

    fn push_consumer_wait(&mut self, waiter: Waiter<T>) -> uint {
        if let ConsumerWait(ref mut vec) = self.state {
            let ret = vec.len();
            vec.push(Some(waiter));
            ret
        } else {
            self.state = ConsumerWait(vec![Some(waiter)]);
            return 0;
        }
    }
}

struct Waiter<T> {
    callback: Box<FnOnce<(),()> + Send>,
}

impl<T: Send> Waiter<T> {
    fn new<F: FnOnce() + Send>(cb: F) -> Waiter<T> {
        let cb = unsafe { currently_scheduled().unwrap().schedule(box cb) };
        Waiter { callback: cb }
    }

    fn invoke(self) {
        self.callback.call_once(());
    }
}

enum State<T> {
    Pending,
    ConsumerWait(Vec<Option<Waiter<T>>>),
    Complete,
    Canceled,
}

impl<T: Send> State<T> {
    fn is_pending(&self) -> bool {
        match *self {
            Pending => true,
            _ => false,
        }
    }

    fn is_canceled(&self) -> bool {
        match *self {
            Canceled => true,
            _ => false,
        }
    }

    fn is_ready(&self) -> bool {
        match *self {
            Complete | Canceled => true,
            _ => false,
        }
    }

    fn is_consumer_wait(&self) -> bool {
        match *self {
            ConsumerWait(..) => true,
            _ => false,
        }
    }
}
