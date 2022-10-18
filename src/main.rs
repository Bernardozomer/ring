#[macro_use]
extern crate lazy_static;

use std::sync::Mutex;

use anyhow::{Result, bail};
use crossbeam::channel::{bounded, Receiver, Sender};
use crossbeam::thread;

const RING_SIZE: usize = 3;

lazy_static! {
    static ref COORDINATOR_ID: Mutex<usize> = Mutex::new(0);
}

fn main() {
    // Create a channel for each ring member.
    let chans: [(Sender<Msg>, Receiver<Msg>); RING_SIZE] = (0..RING_SIZE)
        .map(|_| {
            bounded(1)
        })
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();

    // Create a channel for the simulator.
    let (sim_s, sim_r) = bounded(1);
    // TODO: Read the simulation sequence from a file.
    let sim_seq = SimSeq::default();

    // Spawn a thread for each ring member and one for the controller.
    // Each ring member receives on its channel and sends on the next's.
    thread::scope(|scope| {
        for i in 0..RING_SIZE {
            let (s, r) = (
                match chans.get(i + 1) {
                    Some(next) => next.0.clone(),
                    None => chans[0].0.clone()
                },
                chans[i].1.clone()
            );

            let sim_s = sim_s.clone();

            scope.spawn(
                move |_| RingMember::new(i).election_stage(s, r, sim_s)
            );
        }

        println!("main: election ring created");
        let (first_s, sim_r) = (chans[0].0.clone(), sim_r.clone());
        scope.spawn(move |_| sim_election(sim_seq, first_s, sim_r));
    }).unwrap();

    println!("main: done");
}

fn sim_election(
    seq: SimSeq, first_s: Sender<Msg>, sim_r: Receiver<bool>
) -> Result<()> {
    for (id, secs) in seq.toggles
        .iter()
        // Append a 0 second wait to the wait sequence
        // to get all the ids in the zip.
        .zip(seq.waits.iter().chain(std::iter::repeat(&0)))
    {
        println!("sim: waiting for {:?}s", *secs);
        std::thread::sleep(std::time::Duration::new(*secs, 0));
        first_s.send(Msg::toggle(*id))?;
        println!("sim: toggled {}", *id);
        // Wait for toggle confirmation.
        let active = sim_r.recv()?;

        if !active && *id == *COORDINATOR_ID.lock().unwrap() {
            first_s.send(Msg::election())?;
            println!("sim: election started");
            // Wait for election results.
            sim_r.recv()?;
        }
    }

    first_s.send(Msg::end())?;
    println!("sim: sent end signal");
    println!("sim: done");
    Ok(())
}

#[derive(Debug)]
struct RingMember {
    id: usize,
    active: bool,
}

impl RingMember {
    fn new(id: usize) -> Self {
        Self { id, active: true }
    }

    fn election_stage(
        &mut self, s: Sender<Msg>, r: Receiver<Msg>, sim_s: Sender<bool>
    ) -> Result<()> {
        loop {
            let mut msg = r.recv()?;
            println!("{}: received {:?}", self.id, msg);

            match msg.kind {
                MsgKind::End => {
                    s.send(msg)?;
                    break;
                }
                MsgKind::Toggle => {
                    if !msg.body[self.id] {
                        s.send(msg)?;
                        println!("{}: sent toggle forward", self.id);
                        continue;
                    }

                    self.active ^= true;
                    println!("{}: active = {}", self.id, self.active);
                    sim_s.send(self.active)?;
                    println!("{}: sent toggle to sim", self.id);
                }
                MsgKind::Election => {
                    if !self.active {
                        s.send(msg)?;
                        println!("{}: sent election forward", self.id);
                        continue;
                    }

                    if !msg.body[self.id] {
                        msg.body[self.id] = true;
                        println!("{}: joined election", self.id);
                        s.send(msg)?;
                        println!("{}: sent election forward", self.id);
                        continue;
                    }

                    println!("{}: election ended", self.id);
                    let mut coord_id = COORDINATOR_ID.lock().unwrap();

                    *coord_id = msg.body
                        .iter()
                        .enumerate()
                        // TODO: This sum could overflow. Not nice.
                        .map(|(i, b)| if *b { i } else { RING_SIZE + 1 })
                        .min()
                        .unwrap()
                        as usize;

                    sim_s.send(true)?;
                    println!("{}: sent results to sim", self.id);
                }
            }
        }

        println!("{}: done", self.id);
        Ok(())
    }
}

#[derive(Debug)]
struct Msg {
    kind: MsgKind,
    body: [bool; RING_SIZE],
}

impl Msg {
    fn election() -> Self {
        Self { kind: MsgKind::Election, body: [false; RING_SIZE] }
    }

    fn toggle(id: usize) -> Self {
        let mut body = [false; RING_SIZE];
        body[id] = true;
        Self { kind: MsgKind::Toggle, body }
    }

    fn end() -> Self {
        Self { kind: MsgKind::End, body: [true; RING_SIZE] }
    }
}

#[derive(Debug)]
enum MsgKind {
    Election,
    Toggle,
    End,
}

/// The `SimSeq` type, which specifies a sequence of alternating waits and
/// toggles to be performed by the simulator.
///
/// From start, the simulator should wait for waits[i] seconds and then toggle
/// process toggles[i] active/inactive, in this order, for i = 0 to i = n,
/// such that n is the amount of toggles to be performed.
///
/// Note that the number of toggles must be equal to the number of waits + 1.
#[derive(Debug)]
struct SimSeq {
    /// Ring member ids to be toggles active/inactive.
    toggles: Vec<usize>,
    /// Times in seconds to wait for before each toggle.
    waits: Vec<u64>,
}

impl Default for SimSeq {
    fn default() -> Self {
        let mut toggles = Vec::new();

        // Toggle each member inactive and then active.
        for i in 0..RING_SIZE {
            toggles.push(i);
            toggles.push(i);
        }

        SimSeq::new(
            toggles,
            [1; (RING_SIZE) * 2 - 1].to_vec()
        ).unwrap()
    }
}

impl SimSeq {
    fn new(
        toggles: Vec<usize>, waits: Vec<u64>
    ) -> Result<Self> {
        if toggles.len() != waits.len() + 1 {
            bail!(
                "Number of toggles must be equal to the number of waits + 1"
            );
        }

        Ok(Self { toggles, waits })
    }

    /// Read the simulation sequence from a file.
    /// Waits on odd lines, and toggles on evens.
    fn from_file(path: &std::path::Path) -> Result<Self> {
        unimplemented!()
    }
}
