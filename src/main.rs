static mut LEADER_ID: usize = 0;

fn main() {
    let mut ring = [
        Actor::new(0),
        Actor::new(1),
        Actor::new(2),
        Actor::new(3),
    ];

    loop {
        unimplemented!();
    }
}

#[derive(Debug)]
struct Actor {
    id: usize,
    active: bool,
}

impl Actor {
    pub fn new(id: usize) -> Self {
        Self { id, active: true }
    }
}

#[derive(Debug)]
struct Msg {
    kind: i32,
    body: [i32; 3]
}
