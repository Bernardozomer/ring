use anyhow::Result;
use crossbeam::channel::{bounded, Sender, Receiver};
use crossbeam::sync::WaitGroup;
use crossbeam::thread;

const NUM_ACTORS: usize = 3;

fn main() {
    let wg = WaitGroup::new();
    let (ctrl_sender, ctrl_receiver) = bounded(0);

    let chans: [(Sender<Msg>, Receiver<Msg>); NUM_ACTORS] = [
        bounded(0), bounded(0), bounded(0)
    ];

    let ring: [Actor; NUM_ACTORS] = [
        Actor::new(1), Actor::new(2), Actor::new(3),
    ];

    for (i, actor) in ring.iter().enumerate() {
        let wg = wg.clone();

        thread::scope(|_| {
            actor.election_stage(
                match chans.get(i + 1) {
                    Some(next) => &next.0,
                    None => &chans[0].0,
                },
                &chans[i].1,
                &ctrl_sender
            ).unwrap();

            drop(wg);
        }).unwrap();
    }

    println!("main: created process ring");
    let ctrl_wg = wg.clone();

    thread::scope(|_| {
        control_election(&chans, &ctrl_receiver).unwrap();
        drop(ctrl_wg);
    }).unwrap();

    println!("main: created controller process");
    wg.wait();
}

fn control_election(
    chans: &[(Sender<Msg>, Receiver<Msg>); NUM_ACTORS],
    controller: &Receiver<u8>
) -> Result<()> {
    let msg = Msg {
        kind: 1,
        body: [0; NUM_ACTORS]
    };

    chans[0].0.send(msg)?;
    println!("Control: sent election");
    controller.recv()?;
    println!("Control: received confirmation");
    println!("Control: done");

    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct Actor {
    id: usize,
}

impl Actor {
    pub fn new(id: usize) -> Self {
        Self { id }
    }

    pub fn election_stage(
        self, s: &Sender<Msg>, r: &Receiver<Msg>,
        controller: &Sender<u8>
    ) -> Result<()> {
        let mut msg = r.recv()?;
        println!("{}: received message {:?}", self.id, msg);

        msg.body[self.id] = self.id;
        s.send(msg)?;
        println!("{}: sent message", self.id);

        if self.id == 0 {
            msg = r.recv()?;
            println!("{}: received message {:?}", self.id, msg);
            controller.send(0)?;
            println!("{}: sent confirmation", self.id);
        }

        println!("{}: done", self.id);
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct Msg {
    kind: i32,
    body: [usize; NUM_ACTORS]
}
