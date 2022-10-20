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
                move |_| RingMember::new(i, s, sim_s, r, 0).run()
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
    let mut coord_id = coord_id;

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
        let msg = sim_r.recv()?;

        if let SimMsg::ConfirmToggle { id, active } = msg {
            if id == coord_id && !active {
                first_s.send(Msg::election())?;
                println!("sim: election started");
                // Wait for election results.
                let msg = sim_r.recv()?;

                if let SimMsg::ElectionResult { id } = msg {
                    coord_id = id;
                }
            }
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
    s: Sender<Msg>,
    sim_s: Sender<SimMsg>,
    r: Receiver<Msg>,
    coord_id: usize,
}

impl RingMember {
    fn new(
        id: usize, s: Sender<Msg>, sim_s: Sender<SimMsg>, r: Receiver<Msg>,
        coord_id: usize
    ) -> Self {
        Self { id, active: true, s, sim_s, r, coord_id }
    }

    fn run(&mut self) -> Result<()> {
        loop {
            let msg = self.r.recv()?;
            println!("{}: received {:?}", self.id, msg);
            let res = self.handle_msg(msg)?;

            if !res {
                break;
            }
        }

        println!("{}: done", self.id);
        Ok(())
    }

    fn handle_msg(&mut self, msg: Msg) -> Result<bool> {
        match msg {
            Msg::Election { body } => {
                self.vote(body)?;
                Ok(true)
			}
            Msg::ElectionResult { id } => {
                self.update_coord(id)?;
                Ok(true)
			}
            Msg::Toggle { id } => {
                self.toggle(id)?;
                Ok(true)
			}
            Msg::End => {
                self.s.send(msg)?;
                println!("{}: will now stop", self.id);
                println!("{}: sent stop signal forward", self.id);
                Ok(false)
			}
        }
    }

    /// Vote for the next coordinator or end the election if that has
    /// already been done.
    fn vote(&mut self, mut body: [bool; RING_SIZE]) -> Result<()> {
        if !self.active {
            self.s.send(Msg::Election { body })?;
            println!("{}: sent election forward", self.id);
            return Ok(());
        }

        if !body[self.id] {
            body[self.id] = true;
            let msg = Msg::Election { body };
            self.s.send(msg)?;
            println!("{}: joined election", self.id);
            println!("{}: sent election forward", self.id);
            return Ok(());
        }

        // Elect the ring member with the lowest id who voted.
        let winner_id = body.iter()
            .enumerate()
            .filter(|(_, b)| **b)
            .map(|(i, _)| i)
            .min()
            .unwrap();

        self.s.send(Msg::ElectionResult { id: winner_id })?;
        println!("{}: election ended", self.id);
        println!("{}: sent result forward", self.id);
        Ok(())
    }

    /// Update the coordinator id based on the election results.
    fn update_coord(&mut self, id: usize) -> Result<()> {
        if self.coord_id == id {
            self.sim_s.send(SimMsg::ElectionResult { id })?;
            println!("{}: sent result to sim", self.id);
            return Ok(());
        }

        self.s.send(Msg::ElectionResult { id })?;
        self.coord_id = id;

        println!(
            "{}: {} won the election", self.id, self.coord_id
        );

        println!("{}: sent result forward", self.id);
        Ok(())
    }

    /// Toggle active/inactive if target is self, else send message forward.
    fn toggle(&mut self, id: usize) -> Result<()> {
        if id != self.id {
            self.s.send(Msg::Toggle { id })?;
            println!("{}: sent toggle forward", self.id);
            return Ok(());
        }

        self.active ^= true;

        self.sim_s.send(SimMsg::ConfirmToggle {
            id: self.id,
            active: self.active
        })?;

        println!("{}: active = {}", self.id, self.active);
        println!("{}: sent toggle to sim", self.id);
        Ok(())
    }
}

#[derive(Debug)]
enum Msg {
    Election { body: [bool; RING_SIZE] },
    ElectionResult { id: usize },
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
    ConfirmToggle { id: usize, active: bool },
    ElectionResult { id: usize },
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
    /// Default simulation sequence.
    ///
    /// Toggle the coordinator inactive until the last ring member
    /// is the only one left. Then, toggle its predecessor active before
    /// toggling the coordinator inactive and then active and so on
    /// until the first ring member is reached.
    /// Wait 1 second between toggles.
    ///
    /// E.g.: The toggle order for 0 1 2 is 0 1 1 2 2 0 1 1.
    fn default() -> Self {
        const NUM_TOGGLES: usize = RING_SIZE - 1 + (RING_SIZE - 1) * 3;
        let mut toggles = Vec::with_capacity(NUM_TOGGLES);

        for i in 0..RING_SIZE - 1 {
            toggles.push(i);
        }

        for i in (0..RING_SIZE - 1).rev() {
            toggles.push(i);
            toggles.push(i + 1);
            toggles.push(i + 1);
        }

        SimSeq::new(toggles, [1; NUM_TOGGLES - 1].to_vec()).unwrap()
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
