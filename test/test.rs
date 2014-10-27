#![feature(phase)]
#![feature(unboxed_closures)]

extern crate astaire;

#[phase(plugin, link)]
extern crate log;

mod test_actor_system;
