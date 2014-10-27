//! A variant of std::sync::Arc that allows unsafe manipulation.
use std::mem;
use std::sync::atomic;

#[unsafe_no_drop_flag]
pub struct UnsafeArc<T> {
    __ptr: *mut ArcInner<T>,
}

struct ArcInner<T> {
    count: atomic::AtomicUint,
    data: T,
}

impl<T: Sync + Send> UnsafeArc<T> {
    #[inline]
    pub fn new(data: T) -> UnsafeArc<T> {
        let inner = box ArcInner {
            count: atomic::AtomicUint::new(1),
            data: data,
        };

        UnsafeArc { __ptr: unsafe { mem::transmute(inner) } }
    }

    #[inline]
    fn inner(&self) -> &ArcInner<T> {
        unsafe { &*self.__ptr }
    }
}

impl<T: Sync + Send> Clone for UnsafeArc<T> {
    #[inline]
    fn clone(&self) -> UnsafeArc<T> {
        // Using a relaxed ordering is alright here, as knowledge of the
        // original reference prevents other threads from erroneously deleting
        // the object.
        //
        // As explained in the [Boost documentation][1], Increasing the
        // reference counter can always be done with memory_order_relaxed: New
        // references to an object can only be formed from an existing
        // reference, and passing an existing reference from one thread to
        // another must already provide any required synchronization.
        //
        // [1]: (www.boost.org/doc/libs/1_55_0/doc/html/atomic/usage_examples.html)
        self.inner().count.fetch_add(1, atomic::Relaxed);
        UnsafeArc { __ptr: self.__ptr }
    }
}

impl<T: Send + Sync> Deref<T> for UnsafeArc<T> {
    #[inline]
    fn deref(&self) -> &T {
        &self.inner().data
    }
}

#[unsafe_destructor]
impl<T: Sync + Send> Drop for UnsafeArc<T> {
    fn drop(&mut self) {
        // This structure has #[unsafe_no_drop_flag], so this drop glue may run
        // more than once (but it is guaranteed to be zeroed after the first if
        // it's run more than once)
        if self.__ptr.is_null() { return }

        // Because `fetch_sub` is already atomic, we do not need to synchronize
        // with other threads unless we are going to delete the object. This
        // same logic applies to the below `fetch_sub` to the `weak` count.
        if self.inner().count.fetch_sub(1, atomic::Release) != 1 { return }

        // This fence is needed to prevent reordering of use of the data and
        // deletion of the data. Because it is marked `Release`, the
        // decreasing of the reference count synchronizes with this `Acquire`
        // fence. This means that use of the data happens before decreasing
        // the reference count, which happens before this fence, which
        // happens before the deletion of the data.
        //
        // As explained in the [Boost documentation][1],
        //
        // It is important to enforce any possible access to the object in
        // one thread (through an existing reference) to *happen before*
        // deleting the object in a different thread. This is achieved by a
        // "release" operation after dropping a reference (any access to the
        // object through this reference must obviously happened before),
        // and an "acquire" operation before deleting the object.
        //
        // [1]: (www.boost.org/doc/libs/1_55_0/doc/html/atomic/usage_examples.html)
        atomic::fence(atomic::Acquire);

        // Destroy the data at this time.
        let inner: Box<ArcInner<T>> = unsafe { mem::transmute(self.__ptr) };
        drop(inner);
    }
}
