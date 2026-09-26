use std::fs::{OpenOptions, TryLockError};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant};
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let make = || OpenOptions::new().read(true).write(true).create(true).open(&path).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let child_stop = stop.clone();
    let spawner = std::thread::spawn(move || {
        let mut spawned = 0;
        while !child_stop.load(Ordering::Relaxed) {
            assert!(std::process::Command::new("/bin/zsh").args(["-f", "-c", ":"]).status().unwrap().success());
            spawned += 1;
        }
        spawned
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut attempts = 0;
    let mut transients = 0;
    let mut longest = Duration::ZERO;
    while Instant::now() < deadline {
        let held = make();
        let start = Instant::now();
        loop {
            match held.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {
                    assert!(start.elapsed() < Duration::from_secs(1));
                    std::thread::yield_now();
                }
                Err(e) => panic!("{e}"),
            }
        }
        drop(held);
        let contender = make();
        let start = Instant::now();
        if matches!(contender.try_lock(), Err(TryLockError::WouldBlock)) {
            transients += 1;
            loop {
                match contender.try_lock() {
                    Ok(()) => break,
                    Err(TryLockError::WouldBlock) => {
                        assert!(start.elapsed() < Duration::from_secs(1));
                        std::thread::yield_now();
                    }
                    Err(e) => panic!("{e}"),
                }
            }
            longest = longest.max(start.elapsed());
        }
        drop(contender);
        attempts += 1;
    }
    stop.store(true, Ordering::Relaxed);
    println!("attempts={attempts} spawned={} transient_drop_conflicts={transients} longest={longest:?}", spawner.join().unwrap());
}
