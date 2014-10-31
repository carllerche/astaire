#![crate_name = "astaire"]
#![feature(phase)]
#![feature(if_let)]
#![feature(while_let)]
#![feature(thread_local)]
#![feature(overloaded_calls)]
#![feature(unboxed_closures)]
#![feature(unsafe_destructor)]
#![feature(default_type_params)]

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

pub mod util;

mod core;
mod actor;
mod system;
