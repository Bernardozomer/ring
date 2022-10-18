use anyhow::{Result, bail};
use crossbeam::channel::{bounded, Receiver, Sender};
use crossbeam::thread;

const RING_SIZE: usize = 3;

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
                move |_| RingMember::new(i).election_stage(
                    s, r, sim_s, 0
                )
            );
        }

        println!("main: election ring created");
        let (first_s, sim_r) = (chans[0].0.clone(), sim_r.clone());
        scope.spawn(move |_| sim_election(sim_seq, first_s, sim_r, 0));
    }).unwrap();

    println!("main: done");
}

fn sim_election(
    seq: SimSeq, first_s: Sender<Msg>, sim_r: Receiver<SimMsg>,
    coord_id: usize
) -> Result<()> {
    let coord_id = coord_id;

    for (id, secs) in seq.toggles
        .iter()
        // Append a 0 second wait to the wait sequence
        // to get all the ids in the zip.
        .zip(seq.waits.iter().chain(std::iter::repeat(&0)))
    {
        println!("sim: waiting for {:?}s", *secs);
        std::thread::sleep(std::time::Duration::new(*secs, 0));
        first_s.send(Msg::Toggle { id: *id })?;
        println!("sim: toggled {}", *id);
        // Wait for toggle confirmation.
        let active = sim_r.recv()?;

        if let SimMsg::ConfirmToggle { id } = match sim_r.recv() {
            Ok(it) => it,
            Err(err) => return Err(err),
        };

        if !active && *id == coord_id {
            first_s.send(Msg::election())?;
            println!("sim: election started");
            // Wait for election results.
            sim_r.recv()?;
            // TODO: update coordinator
        }
    }

    first_s.send(Msg::End)?;
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
        &mut self, s: Sender<Msg>, r: Receiver<Msg>, sim_s: Sender<SimMsg>,
        coord_id: usize
    ) -> Result<()> {
        let mut coord_id = coord_id;

        loop {
            let msg = r.recv()?;
            println!("{}: received {:?}", self.id, msg);

            match msg {
                Msg::End => {
                    s.send(msg)?;
                    println!("{}: will now stop", self.id);
                    println!("{}: sent stop signal forward", self.id);
                    break;
                }
                Msg::Toggle { id } => {
                    if id != self.id {
                        s.send(msg)?;
                        println!("{}: sent toggle forward", self.id);
                        continue;
                    }

                    self.active ^= true;
                    sim_s.send(self.active)?;
                    println!("{}: active = {}", self.id, self.active);
                    println!("{}: sent toggle to sim", self.id);
                }
                Msg::MemberElected { id } => {
                    if coord_id == id {
                        sim_s.send(true)?;
                        println!("{}: sent result to sim", self.id);
                        continue;
                    }

                    s.send(msg)?;
                    coord_id = id;

                    println!(
                        "{}: {} won the election", self.id, coord_id
                    );

                    println!("{}: sent result forward", self.id);
                }
                Msg::Election { mut body } => {
                    if !self.active {
                        s.send(msg)?;
                        println!("{}: sent election forward", self.id);
                        continue;
                    }

                    if !body[self.id] {
                        body[self.id] = true;
                        s.send(msg)?;
                        println!("{}: joined election", self.id);
                        println!("{}: sent election forward", self.id);
                        continue;
                    }

                    let winner_id = body.iter()
                        .enumerate()
                        .filter(|(_, b)| **b)
                        .map(|(i, _)| i)
                        .min()
                        .unwrap();

                    s.send(Msg::MemberElected { id: winner_id })?;
                    println!("{}: election ended", self.id);
                    println!("{}: sent result forward", self.id);
                }
            }
        }

        println!("{}: done", self.id);
        Ok(())
    }
}

#[derive(Debug)]
enum Msg {
    Election { body: [bool; RING_SIZE] },
    MemberElected { id: usize },
    Toggle { id: usize },
    End,
}

impl Msg {
    fn election() -> Self {
        Self::Election { body: [false; RING_SIZE] }
    }
}

#[derive(Debug)]
enum SimMsg {
    ConfirmToggle { id: usize },
    ElecionResult { id: usize },
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
