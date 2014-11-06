use astaire::{Actor, ActorRef, System};

struct Simple(Sender<uint>);

impl Actor<uint> for Simple {
    fn receive(&mut self, msg: uint) {
        let Simple(ref tx) = *self;
        debug!("simple actor receiving {}", msg);
        tx.send(msg);
    }
}

#[test]
pub fn test_sending_message_to_simple_actor() {
    let (tx, rx) = channel();

    let sys = System::new();
    let act = sys.spawn(Simple(tx));

    act.send(123u);
    assert_eq!(rx.recv(), 123u);
}
