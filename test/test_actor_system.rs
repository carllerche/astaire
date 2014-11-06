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

/*
#[test]
pub fn test_sending_message_to_lambda_actor() {
    let (tx, rx) = channel();

    let sys = System::new();
    let act = sys.spawn(move |&mut:msg:&'static str| tx.send(msg));

    act.send("hello");
    assert_eq!(rx.recv(), "hello");
}
*/

#[test]
pub fn test_prepare_is_invoked_on_spawn() {
    struct Simple(Sender<&'static str>);

    impl Actor<&'static str> for Simple {
        fn prepare(&mut self) {
            let Simple(ref tx) = *self;
            tx.send("win");
        }

        fn receive(&mut self, _msg: &'static str) {
            let Simple(ref tx) = *self;
            tx.send("fail");
            panic!("nope");
        }
    }

    let (tx, rx) = channel();

    let sys = System::new();
    let _ = sys.spawn(Simple(tx));

    assert_eq!(rx.recv(), "win");
}

#[test]
pub fn test_sending_messages_between_actors() {
    struct Proxy<A>(A);

    impl<A: Actor<uint>> Actor<uint> for Proxy<ActorRef<uint, A>> {
        fn receive(&mut self, msg: uint) {
            let Proxy(ref dst) = *self;
            debug!("proxy actor receiving {}", msg);
            dst.send(msg);
        }
    }

    let (tx, rx) = channel();

    let sys = System::new();
    let act = sys.spawn(Proxy(sys.spawn(Simple(tx))));

    act.send(123u);
    assert_eq!(rx.recv(), 123u);
}

#[test]
pub fn test_sending_to_generic_actors() {
    struct Generic(Sender<String>);

    impl<S: Send + ToString> Actor<S> for Generic {
        fn receive(&mut self, msg: S) {
            let Generic(ref tx) = *self;
            tx.send(msg.to_string());
        }
    }

    let (tx, rx) = channel();

    let sys = System::new();
    let act = sys.spawn(Generic(tx));

    act.send("foo");

    assert_eq!(rx.recv(), "foo".to_string());
}

#[test]
pub fn test_sending_actor_ref_in_message() {
    struct Forward;

    impl<M: Send, A: Actor<M>> Actor<(M, ActorRef<M, A>)> for Forward {
        fn receive(&mut self, (msg, dst): (M, ActorRef<M, A>)) {
            dst.send(msg);
        }
    }

    let (tx, rx) = channel();

    let sys = System::new();
    let act = sys.spawn(Forward);

    act.send((123u, sys.spawn(Simple(tx))));

    assert_eq!(rx.recv(), 123u);
}
