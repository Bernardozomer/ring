use std::collections::HashMap;
use std::time::Duration;

use anyhow::{bail, Error, Result};
use crossbeam::channel::{bounded, Receiver, Sender};
use crossbeam::thread;

use std::env;
use std::fs;
use std::path::Path;

const RING_SIZE: usize = 3;

fn main() {
    // Create a channel for each ring member.
    let chans: [(Sender<Msg>, Receiver<Msg>); RING_SIZE] = (0..RING_SIZE)
        .map(|_| bounded(1))
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();

    // Create a channel for the simulator.
    let (sim_s, sim_r) = bounded(1);
    // Try to read the file from command line arguments
    let args: Vec<String> = env::args().collect();

    let sim_seq = match args.len() {
        2 => SimSeq::from_file(Path::new(&args[1])),
        _ => Ok(SimSeq::default()),
    };

    // Spawn a thread for each ring member and one for the controller.
    // Each ring member receives on its channel and sends on the next's.
    thread::scope(|scope| {
        for i in 0..RING_SIZE {
            let ss: HashMap<usize, Sender<Msg>> = (0..RING_SIZE)
                .map(|j| (j.clone(), chans[j].0.clone()))
                .filter(|(j, _)| *j != i)
                .collect::<HashMap<_, _>>();

            let sim_s = sim_s.clone();
            let r = chans[i].1.clone();
            let next_id = if i == RING_SIZE - 1 { 0 } else { i + 1 };

            scope.spawn(move |_| RingMember::new(i, ss, sim_s, r, next_id, 0).run());
        }

        println!("main: election ring created");
        let (first_s, sim_r) = (chans[0].0.clone(), sim_r.clone());
        scope.spawn(move |_| sim_election(sim_seq.unwrap(), first_s, sim_r, 0));
    })
    .unwrap();

    println!("main: done");
}

fn sim_election(
    seq: SimSeq,
    first_s: Sender<Msg>,
    sim_r: Receiver<SimMsg>,
    coord_id: usize,
) -> Result<()> {
    let mut coord_id = coord_id;

    for (id, secs) in seq
        .toggles
        .iter()
        // Append a 0 second wait to the wait sequence
        // to get all the ids in the zip.
        .zip(seq.waits.iter())
    {
        println!("sim: waiting for {:?}s", *secs);
        std::thread::sleep(std::time::Duration::new(*secs, 0));
        first_s.send(Msg::SimToggle { id: *id })?;
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

    first_s.send(Msg::SimEnd)?;
    println!("sim: sent end signal");
    println!("sim: done");
    Ok(())
}

#[derive(Debug)]
struct RingMember {
    id: usize,
    sim_active: bool,
    ss: HashMap<usize, Sender<Msg>>,
    sim_s: Sender<SimMsg>,
    r: Receiver<Msg>,
    next_id: usize,
    coord_id: usize,
}

impl RingMember {
    fn new(
        id: usize,
        ss: HashMap<usize, Sender<Msg>>,
        sim_s: Sender<SimMsg>,
        r: Receiver<Msg>,
        next_id: usize,
        coord_id: usize,
    ) -> Self {
        Self {
            id,
            sim_active: true,
            ss,
            sim_s,
            r,
            next_id,
            coord_id,
        }
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
            Msg::Ping { s_id } => {
                if !self.sim_active {
                    Ok(true)
                } else {
                    self.ss
                        .get(&s_id)
                        .ok_or(Error::msg("Unknown sender"))?
                        .send(Msg::Pong)?;

                    println!("{}: answered ping from {}", self.id, s_id);
                    Ok(true)
                }
            }
            Msg::Pong => Ok(true),
            Msg::Election { body } => {
                self.vote(body)?;
                Ok(true)
            }
            Msg::ElectionResult { id } => {
                self.update_coord(id)?;
                Ok(true)
            }
            Msg::SimToggle { id } => {
                self.toggle(id)?;
                Ok(true)
            }
            Msg::SimEnd => {
                self.ss
                    .get(&self.next_id)
                    .ok_or(Error::msg("Invalid next member id"))?
                    .send(msg)?;

                println!("{}: will now stop", self.id);
                println!("{}: sent stop signal forward", self.id);
                Ok(false)
            }
        }
    }

    /// Vote for the next coordinator or end the election if that has
    /// already been done.
    fn vote(&mut self, mut body: [bool; RING_SIZE]) -> Result<()> {
        if !self.sim_active && body == [false; RING_SIZE] {
            self.send(Msg::Election { body })?;

            println!("{}: received election from sim, but am inactive!", self.id);

            println!("{}: forwarding election", self.id);
            return Ok(());
        }

        if !body[self.id] {
            body[self.id] = true;
            println!("{}: joined election", self.id);

            let msg = Msg::Election { body };
            let sent = self.send(msg);

            if sent.is_ok() {
                println!("{}: forwarding election", self.id);
                return Ok(());
            }
        }

        // Elect the ring member with the lowest id who voted.
        let winner_id = body
            .iter()
            .enumerate()
            .filter(|(_, b)| **b)
            .map(|(i, _)| i)
            .min()
            .unwrap();

        self.sim_force_send(Msg::ElectionResult { id: winner_id })?;
        println!("{}: election ended", self.id);
        println!("{}: {} won the election", self.id, winner_id);
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

        self.sim_force_send(Msg::ElectionResult { id })?;
        self.coord_id = id;

        println!("{}: {} won the election", self.id, self.coord_id);

        println!("{}: sent result forward", self.id);
        Ok(())
    }

    /// Toggle active/inactive if target is self, else send message forward.
    fn toggle(&mut self, id: usize) -> Result<()> {
        if id != self.id {
            self.sim_force_send(Msg::SimToggle { id })?;
            println!("{}: sent toggle forward", self.id);
            return Ok(());
        }

        self.sim_active ^= true;

        self.sim_s.send(SimMsg::ConfirmToggle {
            id: self.id,
            active: self.sim_active,
        })?;

        println!("{}: active = {}", self.id, self.sim_active);
        println!("{}: sent toggle to sim", self.id);
        Ok(())
    }

    /// Send a message to the first active member ringwise.
    fn send(&mut self, msg: Msg) -> Result<()> {
        let range = (0..RING_SIZE).skip(self.id + 1).chain(0..self.id);

        for i in range {
            // Ping the next member.
            self.ss
                .get(&i)
                .ok_or(Error::msg("Missing sender"))?
                .send(Msg::Ping { s_id: self.id })?;

            println!("{}: pinged {}", self.id, i);

            // Wait again for a response after handling an unexpected message
            // if one was received.
            loop {
                let res = self.r.recv_timeout(Duration::from_millis(1));

                if !res.is_ok() {
                    println!("{}: {} is inactive", self.id, i);
                    break;
                }

                if let Ok(Msg::Pong) = res {
                    self.ss.get(&i).unwrap().send(msg)?;
                    println!("{}: {} is active, sending message", self.id, i);
                    return Ok(());
                }

                self.handle_msg(res.unwrap())?;
            }
        }

        bail!("No response")
    }

    /// Send a message ringwise, starting from the next member,
    /// Regardless of whether they are simulating inactivity or not.
    fn sim_force_send(&self, msg: Msg) -> Result<()> {
        self.ss
            .get(&self.next_id)
            .ok_or(Error::msg("Invalid next member id"))?
            .send(msg)?;

        Ok(())
    }
}

#[derive(Debug)]
enum Msg {
    Ping { s_id: usize },
    Pong,
    Election { body: [bool; RING_SIZE] },
    ElectionResult { id: usize },
    SimToggle { id: usize },
    SimEnd,
}

impl Msg {
    fn election() -> Self {
        Self::Election {
            body: [false; RING_SIZE],
        }
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
/// Note that the number of toggles must be equal to the number of waits.
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

        SimSeq::new(toggles, [1; NUM_TOGGLES].to_vec()).unwrap()
    }
}

impl SimSeq {
    fn new(toggles: Vec<usize>, waits: Vec<u64>) -> Result<Self> {
        if toggles.len() != waits.len(){
            bail!("Number of toggles must be equal to the number of waits");
        }

        Ok(Self { toggles, waits })
    }

    /// Read the simulation sequence from a file
    /// Waits on odd lines, and toggles on evens.
    fn from_file(path: &std::path::Path) -> Result<Self> {
        let contents;
        let mut toggles = Vec::new();
        let mut waits = Vec::new();

        match fs::read_to_string(path) {
            Ok(c) => contents = c,
            Err(e) => panic!("Error reading file: {}", e),
        }

        for (i, char) in contents.chars().enumerate() {
            // Skip newlines or whitespaces
            if char == ' ' || char == '\n' {
                continue;
            }
            // spaces increase i value, so waits are in indexes divisible by 4
            // which are represented as odd lines
            else if char.is_numeric() && i % 4 == 0 {
                waits.push(char.to_digit(10).unwrap() as u64);
            }
            // toggles are in indexes divisible by 2 and not 4
            // which are represented as even lines
            else if char.is_numeric() && i % 2 == 0 {
                toggles.push(char.to_digit(10).unwrap() as usize);
            }
        }

        Ok(SimSeq::new(toggles, waits).unwrap())
    }
}
