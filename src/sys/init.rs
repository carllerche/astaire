use {Actor};

// System's root actor.
pub struct Init;

impl Actor<()> for Init {
    fn receive(&mut self, _:()) {
        // Currently, not expecting to receive any messages
        unimplemented!();
    }
}
