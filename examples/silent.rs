// Failures that leave the railway without a `?`: all of these pass default Clippy.
#![allow(dead_code)]
use std::io;

fn commit() -> Result<u32, io::Error> { Ok(1) }
fn lookup() -> Option<u32> { Some(1) }
fn log_line() -> u32 { 1 }

pub fn silent() -> Result<(), io::Error> {
    let _ = commit();
    commit().ok();
    let n = commit().unwrap();
    let m = commit().expect("db");
    let d = commit().unwrap_or_default();
    println!("{n} {m} {d}");
    Ok(())
}

// None of these lose an error.
pub fn fine() -> Result<u32, io::Error> {
    let _ = log_line();
    let _ = commit()?;
    let a = lookup().unwrap_or_default();
    Ok(a)
}

pub fn unknown_callee(tx: std::sync::mpsc::Sender<u32>) {
    let _ = tx.send(1);
}

pub fn allowed(tx: std::sync::mpsc::Sender<u32>) {
    // errpath: allow(no-swallow) receiver hung up during shutdown
    let _ = tx.send(1);
    let n = commit().unwrap(); // errpath: allow(no-panic) checked at startup
    println!("{n}");
}

pub fn bad_allows(tx: std::sync::mpsc::Sender<u32>) {
    // errpath: allow(no-swallow)
    let _ = tx.send(1);
    // errpath: allow(no-such-rule) typo
    let _ = tx.send(2);
}

pub fn diverges(flag: bool) -> u32 {
    if flag {
        panic!("nope");
    }
    1
}

fn main() {}
