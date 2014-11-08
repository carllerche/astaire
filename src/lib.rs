#![crate_name = "astaire"]
#![feature(phase)]
#![feature(if_let)]
#![feature(while_let)]
#![feature(thread_local)]
#![feature(associated_types)]
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

pub mod util {
    pub use core::future::Async;

    pub mod future {
        pub use core::future::{Future, Completer};
    }
}

mod core;
mod actor;
mod system;
