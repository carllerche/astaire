use super::Future;
use core::rt::Schedule;

use std::mem;
use std::sync::Arc;
use syncbox::locks::{MutexCell, MutexCellGuard};

pub struct Val<T> {
    inner: Option<ValInner<T>>,
}

impl<T: Send> Val<T> {
    // Returns a new future with its producer
    pub fn pair() -> (Val<T>, Producer<T>) {
        let inner = ValInner::new();

        let val = Val { inner: Some(inner.clone()) };
        let producer = Producer { inner: Some(inner) };

        (val, producer)
    }

    pub fn unwrap(mut self) -> T {
        self.inner.take().unwrap().consumer_unwrap()
    }

    /// Maps the future to Val<U> by applying the provided function
    pub fn map<U: Send, F: Send + FnOnce(T) -> U>(self, op: F) -> Val<U> {
        let (ret, p) = Val::pair();
        self.ready(move |:val: Val<T>| p.put(op(val.unwrap())));
        ret
    }
}

impl<T: Send> Future for Val<T> {
    fn ready<F: Send + FnOnce(Val<T>)>(mut self, cb: F) {
        self.inner.take().unwrap().ready(cb);
    }
}

pub struct Producer<T> {
    inner: Option<ValInner<T>>,
}

impl<T: Send> Producer<T> {
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
struct ValInner<T> {
    core: Arc<MutexCell<Core<T>>>,
}

impl<T: Send> ValInner<T> {
    fn new() -> ValInner<T> {
        ValInner {
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

    fn ready<F: Send + FnOnce(Val<T>)>(&self, cb: F) {
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
            cb(Val { inner: Some(inner) });
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
    fn val(&self) -> Val<T> {
        Val { inner: Some(self.clone()) }
    }

    #[inline]
    fn lock(&self) -> MutexCellGuard<Core<T>> {
        self.core.lock()
    }
}

impl<T: Send> Clone for ValInner<T> {
    fn clone(&self) -> ValInner<T> {
        ValInner { core: self.core.clone() }
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

fn currently_scheduled() -> Option<&'static Schedule+'static> {
    unimplemented!()
}

impl<T: Send> Waiter<T> {
    fn new<F: FnOnce() + Send>(cb: F) -> Waiter<T> {
        let cb = currently_scheduled().unwrap().schedule(box cb);
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
