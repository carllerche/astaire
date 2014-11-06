use astaire::{Actor, ActorRef, System};
use astaire::util::future::{Val, Producer};

/*
struct Simple(Sender<uint>);

impl Actor<uint> for Simple {
    fn receive(&mut self, msg: uint) {
        let Simple(ref tx) = *self;
        debug!("simple actor receiving {}", msg);
        tx.send(msg);
    }
}
*/

#[test]
pub fn test_sending_simple_future() {
    let (tx, rx) = channel::<uint>();

    let sys = System::new();
    let one = sys.spawn(move |&mut: p: Producer<uint>| p.put(123u));

    let two = sys.spawn(move |&mut: _| {
        let (f, p) = Val::pair();
        let tx = tx.clone();

        one.send(p);
        f.map(move |:v:uint| tx.send(v));
    });

    two.send("go");

    assert_eq!(rx.recv(), 123u);
}
