#![crate_name = "astaire"]
#![feature(phase)]
#![feature(if_let)]
#![feature(while_let)]
#![feature(thread_local)]
#![feature(unboxed_closures)]
#![feature(unsafe_destructor)]

extern crate syncbox;

#[phase(plugin, link)]
extern crate log;

pub use actor::{
    Actor,
    ActorRef,
};

pub use system::{
    System,
    spawn,
};

mod core;
mod actor;
mod system;
// mod util;
