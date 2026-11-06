// Common wrapper patterns: wrapper closures, `X::Result<T>` aliases,
// tail `Err(e)` in a match, `?` on Option, and fallible methods in impls.
#![allow(dead_code)]

mod anyhow_like {
    pub type Result<T> = std::result::Result<T, Error>;
    #[derive(Debug)]
    pub struct Error;
}
mod io {
    pub type Result<T> = std::result::Result<T, Error>;
    #[derive(Debug)]
    pub struct Error;
}

#[derive(Debug)]
pub struct CommitError;

fn traced<T, E>(_name: &str, f: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    f()
}
fn load(id: u32) -> anyhow_like::Result<u32> {
    Ok(id)
}
fn commit(_v: u32) -> Result<(), CommitError> {
    Ok(())
}

pub fn save(id: u32) -> Result<u32, CommitError> {
    traced("save", || {
        let v = load(id).map_err(|_| CommitError)?;
        match commit(v) {
            Ok(()) => Ok(v),
            Err(e) => Err(e),
        }
    })
}

pub fn first(xs: &[u32]) -> Option<u32> {
    let x = xs.first()?;
    Some(*x)
}

pub struct Listener;
impl Listener {
    fn bind(_port: u16) -> io::Result<Listener> {
        Ok(Listener)
    }
}
pub struct Server;
impl Server {
    pub fn start(port: u16) -> io::Result<Server> {
        let _l = Listener::bind(port)?;
        let _v = std::env::var("PORT").map_err(|_| io::Error)?;
        Ok(Server)
    }
}

fn main() {}
