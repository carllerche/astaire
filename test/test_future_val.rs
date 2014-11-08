use astaire::{Actor, ActorRef, System};
use astaire::util::future::{Future, Completer};

#[test]
pub fn test_sending_simple_future() {
    let (tx, rx) = channel::<uint>();

    struct Simple(Sender<uint>);

    impl<A: Actor<Completer<uint>>> Actor<ActorRef<Completer<uint>, A>> for Simple {
        fn receive(&mut self, other: ActorRef<Completer<uint>, A>) {
            let (f, p) = Future::pair();
            let Simple(ref tx) = *self;
            let tx = tx.clone();

            f.map(move |:v:uint| tx.send(v));
            other.send(p);
        }
    }

    let sys = System::new();
    let one = sys.spawn(move |&mut: p: Completer<uint>| p.put(123u));
    let two = sys.spawn(Simple(tx));

    two.send(one);

    assert_eq!(rx.recv(), 123u);
}

#[test]
#[ignore]
pub fn test_returning_simple_future() {
    use std::mem;

    struct Simple {
        count: uint,
        producer: Option<Completer<uint>>,
    }

    // This error seems to be related to the presence of the return type
    impl Actor<uint, Future<uint>> for Simple {
        fn receive(&mut self, _: uint) -> Future<uint> {
            let (f, p) = Future::pair();

            if let Some(p) = mem::replace(&mut self.producer, Some(p)) {
                self.count += 1;
                p.put(self.count);
            }

            f
        }
    }

    let (tx, rx) = channel::<uint>();

    let sys = System::new();
    let one = sys.spawn(Simple {
        count: 0,
        producer: None
    });

    let two = sys.spawn(move |&mut: _: uint| -> Future<uint> {
        let tx = tx.clone();
        // one.send(1).map(move |:v| tx.send(v));
        unimplemented!()
    });

    /*
    two.send(1u);
    assert_eq!(rx.recv(), 1u);
    */
}
