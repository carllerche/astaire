use astaire::{Actor, ActorRef, System};
use astaire::util::future::{Val, Producer};

#[test]
pub fn test_sending_simple_future() {
    let (tx, rx) = channel::<uint>();

    struct Simple(Sender<uint>);

    impl<A: Actor<Producer<uint>>> Actor<ActorRef<Producer<uint>, A>> for Simple {
        fn receive(&mut self, other: ActorRef<Producer<uint>, A>) {
            let (f, p) = Val::pair();
            let Simple(ref tx) = *self;
            let tx = tx.clone();

            f.map(move |:v:uint| tx.send(v));
            other.send(p);
        }
    }

    let sys = System::new();
    let one = sys.spawn(move |&mut: p: Producer<uint>| p.put(123u));
    let two = sys.spawn(Simple(tx));

    two.send(one);

    assert_eq!(rx.recv(), 123u);
}

#[test]
pub fn test_returning_simple_future() {
    use std::mem;

    struct Simple {
        count: uint,
        producer: Option<Producer<uint>>,
    }

    // This error seems to be related to the presence of the return type
    impl Actor<uint, Val<uint>> for Simple {
        fn receive(&mut self, _: uint) -> Val<uint> {
            let (f, p) = Val::pair();

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

    one.send(1);

    let two = sys.spawn(move |&mut: _: uint| -> Val<uint> {
        let tx = tx.clone();
        one.send(1); // .map(move |:v| tx.send(v));
        unimplemented!()
    });

    /*
    two.send(1u);
    assert_eq!(rx.recv(), 1u);
    */
}
