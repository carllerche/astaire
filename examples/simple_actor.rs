extern crate astaire;

use astaire::{
    Actor,
    System,
};

struct MyActor;

impl Actor<&'static str> for MyActor {
    fn prepare(&mut self) {
        println!("preparing");
    }

    fn receive(&mut self, msg: &'static str) {
        println!("got: {}", msg);
    }
}

pub fn main() {
    use std::io::timer::sleep;
    use std::time::Duration;

    let system = System::new();
    let actor = system.spawn(MyActor);

    actor.send("hello");

    sleep(Duration::milliseconds(100));

    println!("Attempting to shutdown");
    system.shutdown();
}
