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
 * - Transmute *const Cell to *const CellData to access next / prev ptrs
 *
 * ## Spawn Link
 *
 * Send link to actor, even if recipient is not already spawned, it will get
 * linked. On Spawn, the actor will go through all linked children and spawn
 * them.
 *
 * ## TODO
 *
 * Figure out how to deal w/ messages when the actor is terminating.
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
    data: CellData, // Must be first field
    // The state of the actor
    state: CellState,
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
            state: CellState::new(),
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
        let mut curr = self.state.load();

        loop {
            debug!("deliver_event; state={}; spawning={}", curr, spawning);

            let next = match curr.lifecycle() {
                // When in the initial state, only request a schedule when the event is a spawn
                New => return spawning,
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

            self.process_sys_event(event, is_spawned);
        }

        // If is terminating, drain the queue here
        if is_terminated {
            // No need to schedule again
            return false;
        }

        // Process a single user event
        if let Some(event) = self.mailbox.take() {
            self.process_event(event);
            // Before adding batching, it is important to note that it is
            // possible for the cell state to transition to Terminating after
            // processing an event.
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
            Terminate => {
                // Cell has been requested to terminate
                self.terminate();
            }
            ChildTerminated(cell) => {
                // A child actor terminated. Unlink the child
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
        for child in self.child_iter() {
            child.deliver_sys_event(Spawn);
        }
    }

    /// Initialize termination of Cell.
    ///
    /// Dispatches Terminate messages to all
    /// children. If there are no children, completes the termination process.
    /// If there are children, the Cell will wait until all children have
    /// acknowledged the termination before finishing terminating itself.
    fn initialize_termination(&self) {
        for child in self.child_iter() {
            child.deliver_sys_event(Terminate);
        }

        if !self.has_children() {
            self.complete_termination();
        }
    }

    /// Completes termination of the cell and notifies supervisor.
    ///
    /// By this point, all children are terminated. Transitions the state to
    /// Terminated, deallocates the actor (not the Cell) and notifies the
    /// supervisor that the Cell has been terminated.
    fn complete_termination(&self) {
        // First, transition to a terminated state
        let mut curr = self.state.load();

        loop {
            debug!("complete_termination; curr-state={};", curr);

            // The cell must be in the terminating state or something is very
            // wrong.
            assert!(curr.lifecycle() == Terminating, "invalid state {}", curr);

            // Transition
            let actual = self.state.compare_and_swap(curr, curr.with_lifecycle(Terminated));

            if actual == curr {
                break;
            }

            curr = actual;
        }

        // Notify the supervisor that the cell has been terminated
        if let Some(supervisor) = self.supervisor() {
            supervisor.deliver_sys_event(ChildTerminated(self.to_ref()));
        }

        // 3) deallocate actor
    }

    /// Iterate over supervised cells
    fn child_iter<'a>(&'a self) -> ChildIter<'a> {
        ChildIter { next: self.data.children }
    }

    /// True if the cell has child cells to supervise
    fn has_children(&self) -> bool {
        !self.data.children.is_null()
    }

    fn supervisor(&self) -> Option<&(CellObj+Send+Sync)> {
        self.data.supervisor.cell_obj()
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
    // Execute a single iteration of the actor, note that this could happen
    // while the Actor is currently running.
    fn tick(&mut self) -> bool {
        debug!("Cell::tick");

        // Transition the cell to the running state
        let mut before = self.state.load();
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

            let actual = self.state.compare_and_swap(before, next);

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
        let mut curr = self.state.load();
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

            let actual = self.state.compare_and_swap(curr, next);

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

    // self.deliver_sys_event(Link(cell))
    fn deliver_sys_event(&self, event: SysEvent) -> bool {
        CellInner::deliver_sys_event(self, event)
    }

    // Should only be called from context of the running actor. Flags the actor
    // for termination after message has been processed
    fn terminate(&self) {
        let mut curr = self.state.load();

        loop {
            debug!("CellObj::terminate; curr-state={};", curr);

            let next = match curr.lifecycle() {
                Active => curr.with_lifecycle(Terminating),
                Terminating => return,
                New | Terminated => panic!("unexpected state {}", curr),
            };

            let actual = self.state.compare_and_swap(curr, next);

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

        ptr.cell_obj().expect("CellObj should exist at this point")
    }

    fn unlink(&mut self) -> CellRef {
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

    fn push_sibling(&mut self, mut cell: CellPtr) {
        if let Some(curr) = self.cell_data_mut() {
            curr.prev_sibling = cell;
        }

        if let Some(data) = cell.cell_data_mut() {
            data.next_sibling = *self;
        }
    }

    fn cell_obj<'a>(&self) -> Option<&'a (CellObj+Send+Sync)> {
        if self.cell.is_null() {
            return None;
        }

        unsafe {
            let data: &CellData = mem::transmute(self.cell);
            Some(mem::transmute(TraitObject {
                data: mem::transmute(data),
                vtable: data.vtable,
            }))
        }
    }

    fn cell_obj_mut<'a>(&mut self) -> Option<&'a mut (CellObj+Send+Sync)> {
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
