use {Actor};
use self::State::*;
use core::{SysEvent, Event, Runtime, RuntimeWeak};
use core::Event::{Message, Exec};
use core::SysEvent::{Spawn, Link, Terminated};
use core::future::Request;
use util::Async;

use std::cell::UnsafeCell;
use std::{mem, ptr};
use std::num::FromPrimitive;
use std::raw::TraitObject;
use std::sync::atomic::{AtomicUint, Acquire, Relaxed, Release, fence};
use syncbox::{Consume, Produce, LinkedQueue};

/*
 * Strategy
 * - Inline Arc
 * - Split out Cell impl data from actor
 * - Transmute *const Cell to *const CellData to access next / prev ptrs
 *
 * ## Spawn Link
 *
 * Send link to actor, even if recipient is not already spawned, it will get
 * linked. On Spawn, the actor will go through all linked children and spawn
 * them.
 *
 * How does one track whether or not the actor was spawned?
 */
pub struct Cell<A, M: Send, R: Async> {
    inner: *mut CellInner<A, M, R>,
}

impl<Msg: Send, Ret: Async, A: Actor<Msg, Ret>> Cell<A, Msg, Ret> {
    pub fn new(actor: A, runtime: RuntimeWeak) -> Cell<A, Msg, Ret> {
        let inner = CellInner::new(actor, runtime);
        Cell { inner: unsafe { mem::transmute(inner) } }
    }

    pub fn send_request(&self, request: Request<Msg, Ret>) {
        self.inner().send_request(request, self.clone());
    }

    pub fn deliver_event(&self, event: Event<Msg, Ret>) -> bool {
        self.inner().deliver_event(event)
    }

    pub fn deliver_sys_event(&self, event: SysEvent) -> bool {
        self.inner().deliver_sys_event(event)
    }

    pub fn to_ref(&self) -> CellRef {
        self.inner().retain();
        CellRef::new(CellPtr::new(self))
    }

    fn inner(&self) -> &CellInner<A, Msg, Ret> {
        unsafe { &*self.inner }
    }

    fn inner_mut(&mut self) -> &mut CellInner<A, Msg, Ret> {
        unsafe { &mut *self.inner }
    }
}

impl<Msg: Send, Ret: Async, A: Actor<Msg, Ret>> Clone for Cell<A, Msg, Ret> {
    fn clone(&self) -> Cell<A, Msg, Ret> {
        self.inner().handle()
    }
}

#[unsafe_destructor]
impl<Msg: Send, Ret: Async, A: Actor<Msg, Ret>> Drop for Cell<A, Msg, Ret> {
    fn drop(&mut self) {
        unsafe { self.inner_mut().release(); }
    }
}

#[unsafe_no_drop_flag]
pub struct CellRef {
    ptr: CellPtr,
}

impl CellRef {
    fn new(ptr: CellPtr) -> CellRef {
        CellRef { ptr: ptr }
    }

    pub fn deliver_sys_event(&self, event: SysEvent) -> bool {
        self.ptr.cell_obj().deliver_sys_event(event)
    }

    pub fn tick(&mut self) -> bool {
        self.ptr.cell_obj_mut().tick()
    }

    // Get the runtime behind this cell
    pub fn runtime(&self) -> Runtime {
        self.ptr.cell_obj().runtime()
    }

    pub fn schedule(&self, f: Box<FnOnce<(),()> + Send>) -> Box<FnOnce<(),()> + Send> {
        self.ptr.cell_obj().schedule(f)
    }

    fn to_ptr(self) -> CellPtr {
        unsafe { mem::transmute(self) }
    }
}

impl Clone for CellRef {
    fn clone(&self) -> CellRef {
        self.ptr.cell_obj().retain();
        CellRef { ptr: self.ptr }
    }
}

impl Drop for CellRef {
    fn drop(&mut self) {
        // If the obj was already dropped, ptr will be nulled out
        if self.ptr.is_null() { return; }

        // Release the cell
        unsafe { self.ptr.cell_obj().release(); }
    }
}

#[deriving(Show, FromPrimitive, PartialEq)]
enum State {
    New,       // Initial actor state
    Idle,      // Spawned, no pending messages
    Scheduled, // Spawned, scheduled for execution (pending messages)
    Running,   // Spawned, currently processing messages
    Pending,   // Spawned, currently processing messages, more enqueued
    Shutdown,  // Successfuly finished execution
    Crashed,   // Terminated with failure
}

impl State {
}

// Common fns between a concrete cell and a CellRef
trait CellObj: Send + Sync {
    // Spawn the cell as a child of the current cell
    fn deliver_sys_event(&self, event: SysEvent) -> bool;

    // Scheduler tick, returns whether ot not to reschedule for another
    // iteration.
    fn tick(&mut self) -> bool;

    // Schedule the function to execute in the context of this schedulable type
    fn schedule(&self, f: Box<FnOnce<(),()> + Send>) -> Box<FnOnce<(),()> + Send>;

    fn runtime(&self) -> Runtime;

    fn retain(&self);

    // Release the cell (possibly freeing the memory)
    unsafe fn release(&self);
}

// TODO:
// - Use improved mailbox queue. This can be a MPSC queue w/ no waiting since
//   coordination happens externally.
// - Consider whether or not to split it out into a handle and core for better
//   mut safety.
// - The cell needs to know which runtime it is from, but using std::Arc is
//   pretty heavy. Migrate to a version that keeps thread local ref counts.
struct CellInner<A, M: Send, R: Async> {
    // Supervision tree
    data: CellData, // Must be first field
    // The state of the actor
    state: AtomicUint,
    // The number of outstanding ActorRefs
    ref_count: AtomicUint,
    // The runtime that the actor belongs to
    runtime: RuntimeWeak,
    // The mailbox for all user level messages
    mailbox: LinkedQueue<Event<M, R>>,
    // The mailbox for all system messages
    sys_mailbox: LinkedQueue<SysEvent>,
    // The actor that the cell is powering
    actor: UnsafeCell<A>,
}

impl<Msg: Send, Ret: Async, A: Actor<Msg, Ret>> CellInner<A, Msg, Ret> {
    fn new(actor: A, runtime: RuntimeWeak) -> Box<CellInner<A, Msg, Ret>> {
        let mut ret = box CellInner {
            data: CellData::new(),
            state: AtomicUint::new(New as uint),
            ref_count: AtomicUint::new(0),
            runtime: runtime,
            mailbox: LinkedQueue::new(),
            sys_mailbox: LinkedQueue::new(),
            actor: UnsafeCell::new(actor),
        };

        ret.data.vtable = CellPtr::vtable(&*ret);
        ret
    }

    fn handle(&self) -> Cell<A, Msg, Ret> {
        self.retain();
        Cell { inner: unsafe { mem::transmute(self) } }
    }

    /*
     *
     * ===== Actual cell implementation =====
     *
     */

    pub fn send_request(&self, request: Request<Msg, Ret>, cell: Cell<A, Msg, Ret>) {
        debug!("sending message");
        self.runtime().dispatch(cell, Event::message(request));
    }

    // Atomically enqueues the message and returns whether or not the actor
    // should be scheduled.
    //
    // Messages can be delivered before the actor has been spawned, in which
    // case, the actor is not scheduled.
    fn deliver_event(&self, event: Event<Msg, Ret>) -> bool {
        // First enqueue the message:
        self.mailbox.put(event).ok().unwrap();

        // Schedule the actor if needed
        self.maybe_schedule(false)
    }

    fn deliver_sys_event(&self, event: SysEvent) -> bool {
        let spawning = event.is_spawn();

        // First enqueue the message:
        self.sys_mailbox.put(event).ok().unwrap();

        // Schedule the actor if needed
        self.maybe_schedule(spawning)
    }

    fn maybe_schedule(&self, spawning: bool) -> bool {
        let mut expect = self.state.load(Relaxed);

        loop {
            let curr: State = FromPrimitive::from_uint(expect)
                .expect("[BUG] invalid state");

            debug!("deliver_event; state={}; spawning={}", curr, spawning);

            let next = match curr {
                // Cases in which there is never a need to schedule
                Scheduled | Pending | Shutdown | Crashed => return false,
                // When in the initial state, only request a schedule when the event is a spawn
                New => return spawning,
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

    fn process_queue(&mut self, initial: bool) -> bool {
        debug!("Cell::process_queue; initial={}", initial);

        // If this is the first call, the actor has not yet been spawned
        let mut is_spawned = !initial;

        // First process all system events. The Spawn message is guaranteed to
        // be delivered by the time this function is called, however, any
        // number of other messages might also have been delivered. Drain the
        // system event queue first in order to ensure that Spawn is called.
        while let Some(event) = self.sys_mailbox.take() {
            // Track whether the event was a spawn
            if event.is_spawn() {
                is_spawned = true;
            }

            self.process_sys_event(event, is_spawned);
        }

        // Process a single user event
        if let Some(event) = self.mailbox.take() {
            self.process_event(event);
            return true;
        }

        debug!("  no messages to process");
        return false;
    }

    fn process_event(&mut self, event: Event<Msg, Ret>) {
        debug!("processing event; event={}", event);

        match event {
            Message(msg) => self.receive_msg(msg),
            Exec(f) => f.call_once(()),
        }
    }

    fn process_sys_event(&mut self, event: SysEvent, is_spawned: bool) {
        debug!("processing sys event; event={}", event);

        match event {
            Spawn => {
                // First, invoke the actor's prepare fn
                self.actor().prepare();

                // Issue Spawn messages to all child actors
                self.spawn_children();
            }
            Link(cell) => {
                // Spawn the cell as a child of the current actor
                self.link_child(cell, is_spawned);
            }
            Terminated(_) => {
                // A child actor terminated
                unimplemented!();
            }
        }
    }

    fn receive_msg(&self, request: Request<Msg, Ret>) {
        let Request { message, response } = request;
        self.actor().receive(message).link(response);
    }

    // Links the child. If the current actor is already spawned, send a spawn
    // message to the child.
    //
    // Must be invoked from the actor loop
    fn link_child(&mut self, child: CellRef, is_spawned: bool) {
        // Moves ownership of the cell ref into child list and returns a ref to
        // the cell
        let obj = self.data.link_child(child);

        if is_spawned {
            obj.deliver_sys_event(Spawn);
        }
    }

    fn spawn_children(&self) {
        for child in self.data.child_iter() {
            child.deliver_sys_event(Spawn);
        }
    }

    // Pretty hacky
    fn actor<'a>(&'a self) -> &'a mut A {
        use std::mem;

        unsafe {
            let actor: &mut A = mem::transmute(self.actor.get());
            actor
        }
    }

    fn retain(&self) {
        self.ref_count.fetch_add(1, Relaxed);
    }

    // Decrement the ref count
    unsafe fn release(&self) {
        use alloc::heap::deallocate;

        if self.ref_count.fetch_sub(1, Release) != 1 { return }

        fence(Acquire);

        // Cleanly free the memory, the actor field gets cleaned up when the
        // supervisor releases the actor
        drop(ptr::read(&self.mailbox));
        drop(ptr::read(&self.sys_mailbox));

        // Deallocate the memory
        deallocate(mem::transmute(self), mem::size_of::<CellInner<A, Msg, Ret>>(),
                   mem::min_align_of::<CellInner<A, Msg, Ret>>());
    }
}

impl<Msg: Send, Ret: Async, A: Actor<Msg, Ret>> CellObj for CellInner<A, Msg, Ret> {
    // self.deliver_sys_event(Link(cell))
    fn deliver_sys_event(&self, event: SysEvent) -> bool {
        CellInner::deliver_sys_event(self, event)
    }

    // Execute a single iteration of the actor
    fn tick(&mut self) -> bool {
        debug!("Cell::tick");

        // Transition the cell to the running state
        let mut expect = self.state.load(Relaxed);
        let mut initial = false; // Is this the first call

        loop {
            let curr: State = FromPrimitive::from_uint(expect)
                .expect("[BUG] invalid state");

            let next = match curr {
                // If currently running, then nothing to do
                Running | Shutdown | Crashed => return false,
                New => {
                    initial = true;
                    Running
                }
                Scheduled => Running,
                Pending | Idle => panic!("unexpected state {}", curr),
            };

            let actual = self.state.compare_and_swap(expect, next as uint, Relaxed);

            if actual == expect {
                break;
            }

            // CAS failed, try again
            expect = actual;
        }

        let reschedule = self.process_queue(initial);

        // Transition to next state
        let mut expect = self.state.load(Relaxed);

        loop {
            let curr: State = FromPrimitive::from_uint(expect)
                .expect("[BUG] invalid state");

            let next = match curr {
                Pending => Scheduled,
                Running => if reschedule { Scheduled } else { Idle },
                Shutdown | Crashed => return false,
                New | Scheduled | Idle => panic!("unexpected state {}", curr),
            };

            let actual = self.state.compare_and_swap(expect, next as uint, Relaxed);

            if actual == expect {
                return next == Scheduled;
            }

            // CAS failed, try again
            expect = actual
        }
    }

    fn schedule(&self, f: Box<FnOnce<(),()> + Send>) -> Box<FnOnce<(),()> + Send> {
        let cell = self.handle();
        box move |:| { cell.inner().deliver_event(Event::exec(f)); }
    }

    fn runtime(&self) -> Runtime {
        self.runtime.upgrade()
            .expect("[BUG] runtime has been finalized").clone()
    }

    fn retain(&self) {
        CellInner::retain(self);
    }

    unsafe fn release(&self) {
        CellInner::release(self);
    }
}

// Links between cells in the supervision tree
struct CellData {
    vtable: *mut (), // CellInner vtable for Cell+Sync+Send
    supervisor: CellPtr,
    next_sibling: CellPtr,
    prev_sibling: CellPtr,
    children: CellPtr,
}

impl CellData {
    fn new() -> CellData {
        CellData {
            vtable: ptr::null_mut(),
            supervisor: CellPtr::null(),
            next_sibling: CellPtr::null(),
            prev_sibling: CellPtr::null(),
            children: CellPtr::null(),
        }
    }

    fn link_child(&mut self, child: CellRef) -> &(CellObj+Send+Sync) {
        // Convert CellRef -> a ptr, Drop should not be called
        let ptr = child.to_ptr();

        self.children.push_sibling(ptr);
        self.children = ptr;

        ptr.cell_obj()
    }

    fn child_iter<'a>(&'a self) -> ChildIter<'a> {
        ChildIter { next: self.children }
    }
}

struct ChildIter<'a> {
    next: CellPtr,
}

impl<'a> Iterator<&'a (CellObj+Send+Sync)> for ChildIter<'a> {
    fn next(&mut self) -> Option<&'a (CellObj+Send+Sync)> {
        if let Some(data) = self.next.cell_data() {
            let ret = self.next.cell_obj();
            self.next = data.next_sibling;
            return Some(ret);
        }

        None
    }
}

struct CellPtr {
    cell: *const (),
}

impl CellPtr {
    fn new<M: Send, R: Async, A: Actor<M, R>>(cell: &Cell<A, M, R>) -> CellPtr {
        CellPtr { cell: unsafe { mem::transmute(cell.inner()) } }
    }

    fn null() -> CellPtr {
        CellPtr { cell: ptr::null() }
    }

    fn is_null(&self) -> bool {
        self.cell.is_null()
    }

    fn vtable<M: Send, R: Async, A: Actor<M, R>>(cell: &CellInner<A, M, R>) -> *mut () {
        let obj = cell as &(CellObj+Sync+Send);
        let obj: TraitObject = unsafe { mem::transmute(obj) };
        obj.vtable
    }

    fn push_sibling(&mut self, mut cell: CellPtr) {
        if let Some(curr) = self.cell_data_mut() {
            curr.prev_sibling = cell;
        }

        if let Some(data) = cell.cell_data_mut() {
            data.next_sibling = *self;
        }
    }

    // TODO: Should this return Option?
    fn cell_obj<'a>(&self) -> &'a (CellObj+Send+Sync) {
        unsafe {
            let data: &CellData = mem::transmute(self.cell);
            mem::transmute(TraitObject {
                data: mem::transmute(data),
                vtable: data.vtable,
            })
        }
    }

    fn cell_obj_mut<'a>(&mut self) -> &'a mut (CellObj+Send+Sync) {
        unsafe { mem::transmute(self.cell_obj()) }
    }

    fn cell_data<'a>(&self) -> Option<&'a CellData> {
        if self.cell.is_null() {
            return None;
        }

        unsafe { Some(mem::transmute(self.cell)) }
    }

    fn cell_data_mut<'a>(&mut self) -> Option<&'a mut CellData> {
        unsafe { mem::transmute(self.cell_data()) }
    }
}

impl Clone for CellPtr {
    fn clone(&self) -> CellPtr {
        CellPtr { cell: self.cell }
    }
}
