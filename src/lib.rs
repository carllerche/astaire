#![crate_name = "astaire"]
#![feature(globs)]
#![feature(phase)]
#![feature(thread_local)]
#![feature(associated_types)]
#![feature(unboxed_closures)]
#![feature(unsafe_destructor)]
#![feature(default_type_params)]

extern crate syncbox;
extern crate alloc;

#[phase(plugin, link)]
extern crate log;

pub use actor::Actor;
pub use actor_ref::ActorRef;

pub use system::{
    System,
    spawn,
    terminate,
};

mod core;
mod actor;
mod actor_ref;
mod system;
mod sys;
mod util;
