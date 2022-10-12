use anyhow::Result;
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

    // Create a channel for the controller.
    let (ctrl_s, ctrl_r) = bounded(1);

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

            let ctrl_s = ctrl_s.clone();
            scope.spawn(move |_| election_stage(i as i32, s, r, ctrl_s));
        }

        println!("main: election ring created");
        let (first_s, ctrl_r) = (chans[0].0.clone(), ctrl_r.clone());
        scope.spawn(move |_| election_controller(first_s, ctrl_r));
    }).unwrap();

    println!("main: done");
}

fn election_controller(
    s: Sender<Msg>, ctrl_r: Receiver<u8>
) -> Result<()> {
    s.send(Msg { body: [-1; RING_SIZE] })?;
    println!("ctrl: election started");
    ctrl_r.recv()?;
    println!("ctrl: election ended");
    Ok(())
}

fn election_stage(
    id: i32, s: Sender<Msg>, r: Receiver<Msg>, ctrl_s: Sender<u8>
) -> Result<()> {
    let mut msg = r.recv()?;
    println!("{}: received message {:?}", id, msg);
    msg.body[id as usize] = id;
    s.send(msg)?;
    println!("{}: sent message", id);

    if id == 0 {
        let msg = r.recv()?;
        println!("{}: received message {:?}", id, msg);
        ctrl_s.send(0)?;
        println!("{}: sent confirmation to ctrl", id);
    }

    println!("{}: done", id);
    Ok(())
}

#[derive(Debug)]
struct Msg {
    body: [i32; RING_SIZE]
}
