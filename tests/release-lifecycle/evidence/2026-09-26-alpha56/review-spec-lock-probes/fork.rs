use std::fs::{OpenOptions, TryLockError};
unsafe extern "C" {
    fn fork() -> i32;
    fn _exit(code: i32) -> !;
    fn usleep(microseconds: u32) -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let held = OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
    held.try_lock().unwrap();
    let pid = unsafe { fork() };
    assert!(pid >= 0);
    if pid == 0 {
        // Only async-signal-safe libc calls run in this child.
        unsafe { usleep(100_000); _exit(0) }
    }
    drop(held);
    let contender = OpenOptions::new().read(true).write(true).open(&path).unwrap();
    assert!(matches!(contender.try_lock(), Err(TryLockError::WouldBlock)));
    println!("parent File drop: still locked while forked child retains description");
    let mut status = 0;
    assert_eq!(unsafe { waitpid(pid, &mut status, 0) }, pid);
    assert_eq!(status, 0);
    contender.try_lock().unwrap();
    println!("after child exit: immediately acquired");
}
