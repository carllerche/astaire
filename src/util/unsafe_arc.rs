#![allow(dead_code)]

use std::{mem, ptr};
use std::default::Default;
use std::mem::{min_align_of, size_of};
use std::sync::atomic;
use std::fmt::{mod, Show};
use alloc::heap::deallocate;

#[unsafe_no_drop_flag]
pub struct UnsafeArc<T> {
    __ptr: *mut ArcInner<T>,
}

#[unsafe_no_drop_flag]
pub struct Weak<T> {
    __ptr: *mut ArcInner<T>,
}

struct ArcInner<T> {
    strong: atomic::AtomicUint,
    weak: atomic::AtomicUint,
    data: T,
}

impl<T: Sync + Send> UnsafeArc<T> {
    #[inline]
    pub fn new(data: T) -> UnsafeArc<T> {
        // Start the weak pointer count as 1 which is the weak pointer that's
        // held by all the strong pointers (kinda), see std/rc.rs for more info
        let x = box ArcInner {
            strong: atomic::AtomicUint::new(1),
            weak: atomic::AtomicUint::new(1),
            data: data,
        };

        UnsafeArc { __ptr: unsafe { mem::transmute(x) } }
    }

    pub fn unsafe_ptr(&self) -> *const T {
        &self.inner().data as *const T
    }

    pub fn downgrade(&self) -> Weak<T> {
        // See the clone() impl for why this is relaxed
        self.inner().weak.fetch_add(1, atomic::Relaxed);
        Weak { __ptr: self.__ptr }
    }
}

impl<T> UnsafeArc<T> {
    #[inline]
    fn inner(&self) -> &ArcInner<T> {
        // This unsafety is ok because while this arc is alive we're guaranteed
        // that the inner pointer is valid. Furthermore, we know that the
        // `ArcInner` structure itself is `Sync` because the inner data is
        // `Sync` as well, so we're ok loaning out an immutable pointer to
        // these contents.
        unsafe { &*self.__ptr }
    }
}

impl<T> Clone for UnsafeArc<T> {
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
        self.inner().strong.fetch_add(1, atomic::Relaxed);
        UnsafeArc { __ptr: self.__ptr }
    }
}

impl<T> Deref<T> for UnsafeArc<T> {
    #[inline]
    fn deref(&self) -> &T {
        &self.inner().data
    }
}

impl<T: Send + Sync + Clone> UnsafeArc<T> {
    #[inline]
    pub fn make_unique(&mut self) -> &mut T {
        // Note that we hold a strong reference, which also counts as
        // a weak reference, so we only clone if there is an
        // additional reference of either kind.
        if self.inner().strong.load(atomic::SeqCst) != 1 ||
           self.inner().weak.load(atomic::SeqCst) != 1 {
            *self = UnsafeArc::new((**self).clone())
        }
        // This unsafety is ok because we're guaranteed that the pointer
        // returned is the *only* pointer that will ever be returned to T. Our
        // reference count is guaranteed to be 1 at this point, and we required
        // the UnsafeArc itself to be `mut`, so we're returning the only possible
        // reference to the inner data.
        let inner = unsafe { &mut *self.__ptr };
        &mut inner.data
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
        if self.inner().strong.fetch_sub(1, atomic::Release) != 1 { return }

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

        // Destroy the data at this time, even though we may not free the box
        // allocation itself (there may still be weak pointers lying around).
        unsafe { drop(ptr::read(&self.inner().data)); }

        if self.inner().weak.fetch_sub(1, atomic::Release) == 1 {
            atomic::fence(atomic::Acquire);
            unsafe { deallocate(self.__ptr as *mut u8, size_of::<ArcInner<T>>(),
                                min_align_of::<ArcInner<T>>()) }
        }
    }
}

impl<T: Sync + Send> Weak<T> {
    pub fn upgrade(&self) -> Option<UnsafeArc<T>> {
        // We use a CAS loop to increment the strong count instead of a
        // fetch_add because once the count hits 0 is must never be above 0.
        let inner = self.inner();
        loop {
            let n = inner.strong.load(atomic::SeqCst);
            if n == 0 { return None }
            let old = inner.strong.compare_and_swap(n, n + 1, atomic::SeqCst);
            if old == n { return Some(UnsafeArc { __ptr: self.__ptr }) }
        }
    }

    #[inline]
    fn inner(&self) -> &ArcInner<T> {
        // See comments above for why this is "safe"
        unsafe { &*self.__ptr }
    }
}

impl<T: Sync + Send> Clone for Weak<T> {
    #[inline]
    fn clone(&self) -> Weak<T> {
        // See comments in UnsafeArc::clone() for why this is relaxed
        self.inner().weak.fetch_add(1, atomic::Relaxed);
        Weak { __ptr: self.__ptr }
    }
}

#[unsafe_destructor]
impl<T: Sync + Send> Drop for Weak<T> {
    fn drop(&mut self) {
        // see comments above for why this check is here
        if self.__ptr.is_null() { return }

        // If we find out that we were the last weak pointer, then its time to
        // deallocate the data entirely. See the discussion in UnsafeArc::drop() about
        // the memory orderings
        if self.inner().weak.fetch_sub(1, atomic::Release) == 1 {
            atomic::fence(atomic::Acquire);
            unsafe { deallocate(self.__ptr as *mut u8, size_of::<ArcInner<T>>(),
                                min_align_of::<ArcInner<T>>()) }
        }
    }
}

impl<T: PartialEq> PartialEq for UnsafeArc<T> {
    fn eq(&self, other: &UnsafeArc<T>) -> bool { *(*self) == *(*other) }
    fn ne(&self, other: &UnsafeArc<T>) -> bool { *(*self) != *(*other) }
}

impl<T: PartialOrd> PartialOrd for UnsafeArc<T> {
    fn partial_cmp(&self, other: &UnsafeArc<T>) -> Option<Ordering> {
        (**self).partial_cmp(&**other)
    }
    fn lt(&self, other: &UnsafeArc<T>) -> bool { *(*self) < *(*other) }
    fn le(&self, other: &UnsafeArc<T>) -> bool { *(*self) <= *(*other) }
    fn ge(&self, other: &UnsafeArc<T>) -> bool { *(*self) >= *(*other) }
    fn gt(&self, other: &UnsafeArc<T>) -> bool { *(*self) > *(*other) }
}

impl<T: Ord> Ord for UnsafeArc<T> {
    fn cmp(&self, other: &UnsafeArc<T>) -> Ordering { (**self).cmp(&**other) }
}

impl<T: Eq> Eq for UnsafeArc<T> {}

impl<T: fmt::Show> fmt::Show for UnsafeArc<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<T: Default + Sync + Send> Default for UnsafeArc<T> {
    fn default() -> UnsafeArc<T> { UnsafeArc::new(Default::default()) }
}
