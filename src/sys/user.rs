use {Actor};

// System's user actor.
pub struct User;

impl User {
    pub fn new() -> User {
        User
    }
}

impl Actor<()> for User {
    fn receive(&mut self, _:()) {
        // Currently, not expecting to receive any messages
        unimplemented!();
    }
}
