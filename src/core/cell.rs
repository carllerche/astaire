use {Actor};
use self::Lifecycle::*;
use self::Schedule::*;
use core::{SysEvent, Event, Runtime, RuntimeWeak};
use core::Event::{Message, Exec};
use core::SysEvent::{Spawn, Link, Terminate, ChildTerminated};
use core::future::Request;
use util::Async;

use std::cell::UnsafeCell;
use std::{fmt, mem, ptr};
use std::num::FromPrimitive;
use std::raw::TraitObject;
use std::sync::atomic::{AtomicUint, Acquire, Relaxed, Release, fence};
use syncbox::{Consume, Produce, LinkedQueue};

/*
 * Strategy
 * - Inline Arc
 * - Split out Cell impl data from actor
 * - Transmute *const Cell to *const CellCore to access next / prev ptrs
 *
 * ## Spawn Link
 *
 * Send link to actor, even if recipient is not already spawned, it will get
 * linked. On Spawn, the actor will go through all linked children and spawn
 * them.
 *
 * ## TODO
 *
 * - Figure out how to deal w/ messages when the actor is terminating.
 * - Restructure layout to reduce false sharing
 */
pub struct Cell<A, M: Send, R: Async> {
    inner: *mut CellInner<A, M, R>,
}

impl<Msg: Send, Ret: Async, A: Actor<Msg, Ret>> Cell<A, Msg, Ret> {
    pub fn new(actor: A, supervisor: Option<CellRef>, runtime: RuntimeWeak) -> Cell<A, Msg, Ret> {
        let inner = CellInner::new(actor, supervisor, runtime);
        Cell { inner: unsafe { mem::transmute(inner) } }
    }

    pub fn send_request(&self, request: Request<Msg, Ret>) {
        self.inner().send_request(request, self.clone());
    }

    pub fn deliver_event(&self, event: Event<Msg, Ret>) -> bool {
        self.inner().deliver_event(event)
    }

    pub fn supervisor(&self) -> Option<CellRef> {
        let ptr = self.inner().core.supervisor;

        ptr.cell_obj().map(|obj| {
            obj.retain();
            CellRef::new(ptr)
        })
    }

    pub fn to_ref(&self) -> CellRef {
        self.inner().to_ref()
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
        self.cell_obj().deliver_sys_event(event)
    }

    pub fn tick(&mut self) -> bool {
        self.cell_obj_mut().tick()
    }

    // Get the runtime behind this cell
    pub fn runtime(&self) -> Runtime {
        self.cell_obj().runtime()
    }

    pub fn schedule(&self, f: Box<FnOnce<(),()> + Send>) -> Box<FnOnce<(),()> + Send> {
        self.cell_obj().schedule(f)
    }

    fn to_ptr(self) -> CellPtr {
        unsafe { mem::transmute(self) }
    }

    fn cell_obj(&self) -> &(CellObj+Sync+Send) {
        self.ptr.cell_obj().expect("cell pointer is null")
    }

    fn cell_obj_mut(&mut self) -> &mut (CellObj+Sync+Send) {
        self.ptr.cell_obj_mut().expect("cell pointer is null")
    }
}

impl Clone for CellRef {
    fn clone(&self) -> CellRef {
        self.cell_obj().retain();
        CellRef { ptr: self.ptr }
    }
}

impl Drop for CellRef {
    fn drop(&mut self) {
        if let Some(obj) = self.ptr.cell_obj() {
            unsafe { obj.release() };
        }
    }
}

// Common fns between a concrete cell and a CellRef
trait CellObj: Send + Sync {

    // Scheduler tick, returns whether ot not to reschedule for another
    // iteration.
    fn tick(&mut self) -> bool;

    // Spawn the cell as a child of the current cell
    fn deliver_sys_event(&self, event: SysEvent) -> bool;

    // Terminates the actor (must be called in context of the actor)
    fn terminate(&self);

    // Schedule the function to execute in the context of this schedulable type
    fn schedule(&self, f: Box<FnOnce<(),()> + Send>) -> Box<FnOnce<(),()> + Send>;

    // Get the runtime for the cell
    fn runtime(&self) -> Runtime;

    // Tests if the cell obj is the specified ref
    fn eq(&self, other: &CellRef) -> bool;

    fn to_ref(&self) -> CellRef;

    // Increment ref count
    fn retain(&self);

    // Decrement ref count, freeing the cell if 0
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
    core: CellCore, // Must be first field
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
    fn new(actor: A, supervisor: Option<CellRef>, runtime: RuntimeWeak) -> Box<CellInner<A, Msg, Ret>> {
        let mut ret = box CellInner {
            core: CellCore::new(supervisor),
            ref_count: AtomicUint::new(0),
            runtime: runtime,
            mailbox: LinkedQueue::new(),
            sys_mailbox: LinkedQueue::new(),
            actor: UnsafeCell::new(actor),
        };

        ret.core.vtable = CellPtr::vtable(&*ret);
        ret
    }

    fn handle(&self) -> Cell<A, Msg, Ret> {
        self.retain();
        Cell { inner: unsafe { mem::transmute(self) } }
    }

    fn to_ref(&self) -> CellRef {
        self.retain();
        CellRef::new(CellPtr::new(self))
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
        self.core.maybe_schedule(false)
    }

    fn deliver_sys_event(&self, event: SysEvent) -> bool {
        debug!("deliver_sys_event; event={}", event);

        let spawning = event.is_spawn();

        // First enqueue the message:
        self.sys_mailbox.put(event).ok().unwrap();

        // Schedule the actor if needed
        self.core.maybe_schedule(spawning)
    }

    fn process_mailbox(&mut self, mut is_spawned: bool, mut is_terminated: bool) -> bool {
        debug!("Cell::process_mailbox; is_spawned={}; is_terminated={}", is_spawned, is_terminated);

        // First process all system events. The Spawn message is guaranteed to
        // be delivered by the time this function is called, however, any
        // number of other messages might also have been delivered. Drain the
        // system event queue first in order to ensure that Spawn is called.
        while let Some(event) = self.sys_mailbox.take() {
            match event {
                Spawn => is_spawned = true,
                Terminate => is_terminated = true,
                _ => {}
            }

            self.process_sys_message(event, is_spawned, is_terminated);
        }

        // If is terminating, drain the queue here
        if is_terminated {
            // No need to schedule again
            return false;
        }

        // Process a single user event
        if let Some(event) = self.mailbox.take() {
            self.process_message(event);
            // Before adding batching, it is important to note that it is
            // possible for the cell state to transition to Terminating after
            // processing an event.
            return true;
        }

        debug!("  no messages to process");
        return false;
    }

    fn process_message(&mut self, event: Event<Msg, Ret>) {
        debug!("processing message; message={}", event);

        match event {
            Message(msg) => self.receive_msg(msg),
            Exec(f) => f.call_once(()),
        }
    }

    fn process_sys_message(&mut self, event: SysEvent, is_spawned: bool, is_terminated: bool) {
        debug!("processing sys event; event={}", event);

        match event {
            Spawn => {
                self.transition_to_active();

                // First, invoke the actor's prepare fn
                self.actor().prepare();

                // Issue Spawn messages to all child actors
                self.spawn_children();
            }
            Link(cell) => {
                // Spawn the cell as a child of the current actor
                self.link_child(cell, is_spawned);
            }
            Terminate => {
                // Cell has been requested to terminate
                self.terminate();
            }
            ChildTerminated(child) => {
                // A child actor terminated. Unlink the child
                self.unlink_child(child, is_terminated);
            }
        }
    }

    fn receive_msg(&self, request: Request<Msg, Ret>) {
        let Request { message, response } = request;
        self.actor().receive(message).link(response);
    }

    fn transition_to_active(&self) {
        let mut curr = self.core.state.load();

        loop {
            debug!("Cell::transition_to_active; curr-state={};", curr);

            let next = match curr.lifecycle() {
                New => curr.with_lifecycle(Active),
                _ => panic!("unexpected state {}", curr),
            };

            let actual = self.core.state.compare_and_swap(curr, next);

            if actual == curr {
                return;
            }

            curr = actual;
        }
    }

    // Links the child. If the current actor is already spawned, send a spawn
    // message to the child.
    //
    // Must be invoked from the actor loop
    fn link_child(&mut self, child: CellRef, is_spawned: bool) {
        // Moves ownership of the cell ref into child list and returns a ref to
        // the cell
        if is_spawned {
            let child_ref = self.core.link_child(child).to_ref();

            self.runtime().sys_dispatch(child_ref, Spawn);
        } else {
            self.core.link_child(child);
        }
    }

    fn unlink_child(&mut self, child: CellRef, is_terminated: bool) {
        self.core.unlink_child(child);

        if is_terminated && !self.core.has_children() {
            self.complete_termination();
        }
    }

    fn spawn_children(&self) {
        for child in self.core.child_iter() {
            self.runtime().sys_dispatch(child.to_ref(), Spawn);
        }
    }

    /// Initialize termination of Cell.
    ///
    /// Dispatches Terminate messages to all
    /// children. If there are no children, completes the termination process.
    /// If there are children, the Cell will wait until all children have
    /// acknowledged the termination before finishing terminating itself.
    fn initialize_termination(&mut self) {
        for child in self.core.child_iter() {
            self.runtime().sys_dispatch(child.to_ref(), Terminate);
        }

        if !self.core.has_children() {
            self.complete_termination();
        }
    }

    /// Completes termination of the cell and notifies supervisor.
    ///
    /// By this point, all children are terminated. Transitions the state to
    /// Terminated, deallocates the actor (not the Cell) and notifies the
    /// supervisor that the Cell has been terminated.
    fn complete_termination(&mut self) {
        // TODO: Move part into Core

        // First, transition to a terminated state
        let mut curr = self.core.state.load();

        loop {
            debug!("complete_termination; curr-state={};", curr);

            // The cell must be in the terminating state or something is very
            // wrong.
            assert!(curr.lifecycle() == Terminating, "invalid state {}", curr);

            // Transition
            let actual = self.core.state.compare_and_swap(curr, curr.with_lifecycle(Terminated));

            if actual == curr {
                break;
            }

            curr = actual;
        }

        // Notify the supervisor that the cell has been terminated. All cells
        // will have a supervisor except for the Init actor.
        if let Some(supervisor) = self.supervisor() {
            self.runtime().sys_dispatch(supervisor.to_ref(), ChildTerminated(self.to_ref()));
        }

        // Drop the actor. After this point, the actor field is uninitialized
        // memory and should not be used again.
        self.release_actor();
    }

    fn supervisor(&self) -> Option<&(CellObj+Send+Sync)> {
        self.core.supervisor.cell_obj()
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

    fn release_actor(&mut self) {
        unsafe { drop(ptr::read(&self.actor)); }
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
    // Execute a single iteration of the actor, note that this could happen
    // while the Actor is currently running.
    fn tick(&mut self) -> bool {
        debug!("Cell::tick");

        // Transition the cell to the running state
        let mut before = self.core.state.load();
        let mut next;

        loop {
            // Transition the schedule state
            next = match before.schedule() {
                // If the cell is currently being executed on another thread,
                // abort processing
                Running | Pending => return false,
                Scheduled => before.with_schedule(Running),
                Idle => panic!("unexpected state {}", before),
            };

            let actual = self.core.state.compare_and_swap(before, next);

            if actual == before {
                break;
            }

            before = actual;
        }

        // Process the actor's mailbox, returns whether the actor needs to be
        // rescheduled (more work to be done)
        let reschedule = self.process_mailbox(
            before.lifecycle() != New,   // Whether the actor has been spawned already
            next.lifecycle() != Active); // Whether the actor is terminating / terminated

        // Transition to next state
        let mut curr = self.core.state.load();
        let mut next;

        loop {
            next = match curr.schedule() {
                Pending => curr.with_schedule(Scheduled),
                Running => {
                    if reschedule {
                        curr.with_schedule(Scheduled)
                    } else {
                        curr.with_schedule(Idle)
                    }
                }
                Idle | Scheduled => panic!("unexpected state {}", curr),
            };

            let actual = self.core.state.compare_and_swap(curr, next);

            if actual == curr {
                break;
            }

            // CAS failed, try again
            curr = actual
        }

        if curr.lifecycle() == Terminating && before.lifecycle() != Terminating {
            // The actor signaled that it wishes to terminate. Begin
            // termination process
            self.initialize_termination();
        }

        next.schedule() == Scheduled
    }

    fn deliver_sys_event(&self, event: SysEvent) -> bool {
        CellInner::deliver_sys_event(self, event)
    }

    // Should only be called from context of the running actor. Flags the actor
    // for termination after message has been processed
    fn terminate(&self) {
        let mut curr = self.core.state.load();

        loop {
            debug!("CellObj::terminate; curr-state={};", curr);

            let next = match curr.lifecycle() {
                Active => curr.with_lifecycle(Terminating),
                Terminating => return,
                New | Terminated => panic!("unexpected state {}", curr),
            };

            let actual = self.core.state.compare_and_swap(curr, next);

            if actual == curr {
                return;
            }

            curr = actual;
        }
    }

    fn schedule(&self, f: Box<FnOnce<(),()> + Send>) -> Box<FnOnce<(),()> + Send> {
        let cell = self.handle();
        box move |:| { cell.inner().deliver_event(Event::exec(f)); }
    }

    fn eq(&self, other: &CellRef) -> bool {
        unsafe {
            let a: *const () = mem::transmute(self);
            let b: *const () = mem::transmute(other.ptr);

            a == b
        }
    }

    fn to_ref(&self) -> CellRef {
        CellInner::to_ref(self)
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
// TODO: Minimize false sharing
struct CellCore {
    // The state of the actor
    state: CellState,
    vtable: *mut (), // CellInner vtable for Cell+Sync+Send
    supervisor: CellPtr,
    next_sibling: CellPtr,
    prev_sibling: CellPtr,
    children: CellPtr,
}

impl CellCore {
    fn new(supervisor: Option<CellRef>) -> CellCore {
        let supervisor = supervisor
            .map(|cref| cref.to_ptr())
            .unwrap_or(CellPtr::null());

        CellCore {
            state: CellState::new(),
            vtable: ptr::null_mut(),
            supervisor: supervisor,
            next_sibling: CellPtr::null(),
            prev_sibling: CellPtr::null(),
            children: CellPtr::null(),
        }
    }

    fn maybe_schedule(&self, spawning: bool) -> bool {
        let mut curr = self.state.load();

        loop {
            debug!("  transition to schedule; state={}; spawning={}", curr, spawning);

            let next = match curr.lifecycle() {
                New => {
                    if !spawning { return false; }
                    curr.with_schedule(Scheduled)
                }
                Active | Terminating => {
                    // TODO: Consider not scheduling the actor on normal
                    // messages when terminating, instead discard the message
                    // immediately
                    match curr.schedule() {
                        Idle => curr.with_schedule(Scheduled),
                        Running => curr.with_schedule(Pending),
                        Scheduled | Pending => return false,
                    }
                }
                Terminated => return false,
            };

            let actual = self.state.compare_and_swap(curr, next);

            if actual == curr {
                match next.schedule() {
                    Scheduled => return true,
                    _ => return false,
                }
            }

            curr = actual;
        }
    }

    /// Iterate over supervised cells
    fn child_iter<'a>(&'a self) -> ChildIter<'a> {
        ChildIter { next: self.children }
    }

    /// True if the cell has child cells to supervise
    fn has_children(&self) -> bool {
        !self.children.is_null()
    }

    /// Pushes the cell onto the head of the supervised cells linked list.
    ///
    /// Must be called from the supervisor,
    fn link_child(&mut self, child: CellRef) -> &(CellObj+Send+Sync) {
        // Convert CellRef -> a ptr, Drop should not be called
        let mut ptr = child.to_ptr();

        // Set the current head's previous ptr to the new cell
        if let Some(head) = self.children.cell_core_mut() {
            head.prev_sibling = ptr;
        }

        // Set the new cell's next ptr to the current head
        ptr.cell_core_mut().expect("CellPtr should not be null")
            .next_sibling = self.children;

        // Set the entry pointer to the new cell
        self.children = ptr;

        // Return a ref to the new cell
        ptr.cell_obj().expect("CellObj should exist at this point")
    }

    /// Removes the child cell from the list of supervised cells.
    ///
    /// Must be called from the supervisor.
    fn unlink_child(&mut self, mut child: CellRef) -> CellRef {
        let child_core = child.ptr.cell_core_mut().expect("CellRef should not be null");

        if let Some(next) = child_core.next_sibling.cell_core_mut() {
            next.prev_sibling = child_core.prev_sibling;
        }

        match child_core.prev_sibling.cell_core_mut() {
            Some(prev) => prev.next_sibling = child_core.next_sibling,
            None => self.children = child_core.next_sibling,
        }

        CellRef::new(child.ptr)
    }
}

impl Drop for CellCore {
    fn drop(&mut self) {
        assert!(self.children.is_null(), "cell still has children");
        assert!(self.next_sibling.is_null(), "cell still has sibling pointers");
        assert!(self.prev_sibling.is_null(), "cell still has sibling pointers");

        drop(CellRef::new(self.supervisor));
    }
}

struct ChildIter<'a> {
    next: CellPtr,
}

impl<'a> Iterator<&'a (CellObj+Send+Sync)> for ChildIter<'a> {
    fn next(&mut self) -> Option<&'a (CellObj+Send+Sync)> {
        if let Some(core) = self.next.cell_core() {
            let ret = self.next.cell_obj();
            self.next = core.next_sibling;
            return ret;
        }

        None
    }
}

struct CellPtr {
    cell: *const (),
}

impl CellPtr {
    fn new<M: Send, R: Async, A: Actor<M, R>>(cell: &CellInner<A, M, R>) -> CellPtr {
        CellPtr { cell: unsafe { mem::transmute(cell) } }
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

    fn cell_obj<'a>(&self) -> Option<&'a (CellObj+Send+Sync)> {
        if self.cell.is_null() {
            return None;
        }

        unsafe {
            let core: &CellCore = mem::transmute(self.cell);
            Some(mem::transmute(TraitObject {
                data: mem::transmute(core),
                vtable: core.vtable,
            }))
        }
    }

    fn cell_obj_mut<'a>(&mut self) -> Option<&'a mut (CellObj+Send+Sync)> {
        unsafe { mem::transmute(self.cell_obj()) }
    }

    fn cell_core<'a>(&self) -> Option<&'a CellCore> {
        if self.cell.is_null() {
            return None;
        }

        unsafe { Some(mem::transmute(self.cell)) }
    }

    fn cell_core_mut<'a>(&mut self) -> Option<&'a mut CellCore> {
        unsafe { mem::transmute(self.cell_core()) }
    }
}

impl Clone for CellPtr {
    fn clone(&self) -> CellPtr {
        CellPtr { cell: self.cell }
    }
}

struct CellState {
    state: AtomicUint,
}

impl CellState {
    fn new() -> CellState {
        CellState {
            state: AtomicUint::new(State::new().as_uint()),
        }
    }

    fn load(&self) -> State {
        State { val: self.state.load(Relaxed) }
    }

    fn compare_and_swap(&self, old: State, new: State) -> State {
        let curr = self.state.compare_and_swap(old.as_uint(), new.as_uint(), Relaxed);
        State::load(curr)
    }
}

#[deriving(PartialEq)]
struct State {
    // Bits 0~3 -> ScheduleState
    // Bits 4~7 -> Lifecycle
    val: uint,
}

const SCHEDULE_MASK: uint = 3;

const LIFECYCLE_OFFSET: uint = 2;
const LIFECYCLE_MASK: uint = 3 << LIFECYCLE_OFFSET;

impl State {
    fn new() -> State {
        let val = Schedule::Idle as uint | Lifecycle::New as uint;
        State { val: val }
    }

    fn load(val: uint) -> State {
        State { val: val }
    }

    fn schedule(&self) -> Schedule {
        FromPrimitive::from_uint(self.val & SCHEDULE_MASK)
            .expect("unexpected state value")
    }

    fn with_schedule(&self, schedule: Schedule) -> State {
        let val = self.val & !SCHEDULE_MASK | schedule as uint;
        State { val: val }
    }

    fn lifecycle(&self) -> Lifecycle {
        FromPrimitive::from_uint(self.val & LIFECYCLE_MASK)
            .expect("unexpected state value")
    }

    fn with_lifecycle(&self, lifecycle: Lifecycle) -> State {
        let val = self.val & !LIFECYCLE_MASK | lifecycle as uint;
        State { val: val }
    }

    fn as_uint(&self) -> uint {
        self.val
    }
}

impl fmt::Show for State {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "{} | {}", self.schedule(), self.lifecycle())
    }
}

#[deriving(Show, FromPrimitive, PartialEq)]
enum Schedule {
    Idle      = 0,
    Scheduled = 1,
    Running   = 2,
    Pending   = 3,
}

#[deriving(Show, FromPrimitive, PartialEq)]
enum Lifecycle {
    New         = 0 << LIFECYCLE_OFFSET,
    Active      = 1 << LIFECYCLE_OFFSET,
    Terminating = 2 << LIFECYCLE_OFFSET,
    Terminated  = 3 << LIFECYCLE_OFFSET,
}

#[cfg(test)]
mod test {
    use {actor_ref, Actor};
    use core::{Runtime, RuntimeWeak};
    use core::SysEvent::*;
    use util::Async;
    use super::{Cell, CellRef, State};
    use super::Lifecycle::*;
    use super::Schedule::*;

    #[test]
    pub fn test_basic_cell_operation() {
        let cell = new_cell(MyActor);
        let mut supervisor = cell.supervisor().unwrap();

        // The state of the cell starts off as New
        assert!(cell.state().lifecycle() == New);

        // The supervisor should not contain the child
        assert!(!supervisor.has_child(&cell.to_ref()));

        // Initialize the supervisor and have it spawn the child actor
        assert!(supervisor.deliver_sys_event(Spawn));
        assert!(!supervisor.deliver_sys_event(Link(cell.to_ref())));
        assert!(!supervisor.tick());

        // The supervisor should transition to the Active state
        assert!(supervisor.state().lifecycle() == Active, "actual={}", supervisor.state());

        // The supervisor should contain the child
        assert!(supervisor.has_child(&cell.to_ref()));

        // MyActor should be scheduled for execution, but still in new state
        assert!(cell.state().lifecycle() == New, "actual={}", cell.state());
        assert!(cell.state().schedule() == Scheduled, "actual={}", cell.state());

        // The actor should transition to Active
        assert!(!cell.to_ref().tick());
        assert!(cell.state().lifecycle() == Active, "actual={}", cell.state());
        assert!(cell.state().schedule() == Idle, "actual={}", cell.state());
    }

    #[test]
    pub fn test_receiving_terminate_before_spawn() {
    }

    /*
     *
     * ===== Helpers =====
     *
     */

    struct MyActor;

    impl Actor<uint> for MyActor {
        fn receive(&mut self, msg: uint) {
            println!("Got {}", msg);
        }
    }

    trait CellExt {
        fn state(&self) -> State;
        fn has_child(&self, cell: &CellRef) -> bool;
    }

    impl<M: Send, R: Async, A: Actor<M, R>> CellExt for Cell<A, M, R> {
        fn state(&self) -> State {
            self.inner().core.state.load()
        }

        fn has_child(&self, child: &CellRef) -> bool {
            self.to_ref().has_child(child)
        }
    }

    impl CellExt for CellRef {
        fn state(&self) -> State {
            self.ptr.cell_core().unwrap()
                .state.load()
        }

        fn has_child(&self, cell: &CellRef) -> bool {
            let mut iter = self.ptr.cell_core().unwrap().child_iter();

            for child in iter {
                if child.eq(cell) {
                    return true;
                }
            }

            false
        }
    }

    fn new_cell<M: Send, R: Async, A: Actor<M, R>>(actor: A) -> Cell<A, M, R> {
        let rt = runtime_weak();
        let supervisor = rt.upgrade().unwrap().user_ref()
            .map(|aref| actor_ref::to_ref(&aref)).unwrap();

        Cell::new(actor, Some(supervisor), rt)
    }

    fn runtime_weak() -> RuntimeWeak {
        thread_local!(static RUNTIME: Runtime = Runtime::new());
        RUNTIME.with(|rt| rt.weak())
    }
}
