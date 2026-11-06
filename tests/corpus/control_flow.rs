// Parsed, never compiled: one of each failure-path shape, for src/mutation_tests.rs.

fn fetch(id: u32) -> Result<u32, FetchError> { Ok(id) }
fn store(v: u32) -> Result<(), StoreError> { Ok(()) }
fn find(id: u32) -> Option<u32> { Some(id) }

struct Repo;
impl Repo {
    fn load(&self, id: u32) -> Result<u32, FetchError> { fetch(id) }

    fn save(&self, id: u32) -> Result<u32, AppError> {
        let v = self.load(id).map_err(AppError::Fetch)?;
        store(v).map_err(AppError::Store)?;
        Ok(v)
    }
}

fn linear(id: u32) -> Result<u32, AppError> {
    let a = fetch(id).map_err(AppError::Fetch)?;
    let b = fetch(a).context("second fetch")?;
    store(b).map_err(|e| AppError::Store(e))?;
    Ok(b)
}

fn branches(id: u32, retry: bool) -> Result<u32, AppError> {
    let v = fetch(id).map_err(AppError::Fetch)?;
    if v > 10 {
        return Err(AppError::TooBig);
    }
    if retry && v == 0 {
        store(v).map_err(AppError::Store)?;
    } else if retry {
        fetch(v).map_err(AppError::Fetch)?;
    } else {
        return Ok(0);
    }
    Ok(v)
}

fn short_circuit(a: bool, id: u32) -> Result<bool, AppError> {
    let x = a && fetch(id).map_err(AppError::Fetch)? > 1;
    let y = a || check(id)?;
    Ok(x && y)
}

fn matches(id: u32, kind: Kind) -> Result<u32, AppError> {
    let v = match kind {
        Kind::Card if id > 3 => fetch(id).map_err(AppError::Fetch)?,
        Kind::Card => return Err(AppError::Declined),
        Kind::Wallet(0) => 0,
        Kind::Wallet(n) if allowed(n)? => n,
        _ => fetch(0).map_err(AppError::Fetch)?,
    };
    match store(v) {
        Ok(()) => Ok(v),
        Err(e) => Err(AppError::Store(e)),
    }
}

fn let_else(x: Option<u32>, y: Option<u32>) -> Result<u32, AppError> {
    let Some(0) = x else {
        return Err(AppError::Missing);
    };
    let Some(n) = y.filter(|v| *v > 1) else {
        return Ok(1);
    };
    fetch(n).map_err(AppError::Fetch)
}

fn loops(ids: Vec<u32>, stop: bool) -> Result<u32, AppError> {
    let mut total = 0;
    for id in ids.iter() {
        if stop {
            break;
        }
        if *id == 0 {
            continue;
        }
        total += fetch(*id).map_err(AppError::Fetch)?;
    }
    while total < 10 {
        total += fetch(total).map_err(AppError::Fetch)?;
    }
    'outer: for a in ids.iter() {
        for b in ids.iter() {
            if a == b {
                continue 'outer;
            }
            store(*b).map_err(AppError::Store)?;
        }
    }
    loop {
        if total > 0 {
            break;
        }
        store(total).map_err(AppError::Store)?;
    }
    Ok(total)
}

fn optional(id: u32) -> Option<u32> {
    let a = find(id)?;
    if a == 0 {
        return None;
    }
    let b = find(a)?;
    Some(b)
}

fn optional_tail(id: u32) -> Option<u32> {
    if id > 1 {
        Some(id)
    } else {
        None
    }
}

fn silent(id: u32, tx: Sender<u32>) -> Result<u32, AppError> {
    let _ = fetch(id);
    fetch(id).ok();
    let a = fetch(id).unwrap();
    let b = fetch(a).expect("second");
    let c = fetch(b).unwrap_or_default();
    let _ = tx.send(c);
    Ok(c)
}

fn panics(id: u32) -> u32 {
    if id == 0 {
        panic!("zero");
    }
    match id {
        1 => todo!(),
        2 => unreachable!(),
        n => n,
    }
}

fn closures(ids: Vec<u32>) -> Result<Vec<u32>, AppError> {
    traced("load", || {
        let v = fetch(1).map_err(AppError::Fetch)?;
        store(v).map_err(AppError::Store)?;
        Ok(v)
    })?;
    spawn(async move {
        store(2).map_err(AppError::Store)?;
        Ok(())
    });
    ids.into_iter().map(|id| fetch(id).map_err(AppError::Fetch)).collect()
}

async fn awaited(id: u32) -> Result<u32, AppError> {
    let a = remote(id).await.map_err(AppError::Fetch)?;
    if a > 1 {
        remote(a).await.map_err(AppError::Fetch)?;
    }
    Ok(a)
}

fn recovery(id: u32) -> Result<u32, AppError> {
    let a = fetch(id).or_else(|_| fetch(0)).map_err(AppError::Fetch)?;
    let b = find(a).ok_or(AppError::Missing)?;
    if let Err(e) = store(b) {
        log(e);
    }
    Ok(b)
}

trait Store {
    fn put(&self, v: u32) -> Result<(), StoreError>;

    fn put_twice(&self, v: u32) -> Result<(), StoreError> {
        self.put(v)?;
        self.put(v)
    }
}

fn converted_later(id: u32) -> Result<u32, AppError> {
    let r = fetch(id).map_err(AppError::Fetch);
    log(r.is_ok());
    let s = wrap(fetch(id).map_err(AppError::Fetch)).boxed();
    r
}

fn jump_only_inner_loop(rows: Vec<Vec<u32>>) -> Result<u32, AppError> {
    let mut seen = 0;
    'rows: for row in rows.iter() {
        for cell in row.iter() {
            if *cell == 0 {
                break 'rows;
            }
            if *cell == 1 {
                continue 'rows;
            }
        }
        seen += fetch(1).map_err(AppError::Fetch)?;
    }
    Ok(seen)
}

fn fallible_conditions(id: u32) -> Result<u32, AppError> {
    let mut n = 0;
    while ready(id)? {
        n += fetch(n).map_err(AppError::Fetch)?;
    }
    while let Some(v) = next_value(n)? {
        store(v).map_err(AppError::Store)?;
    }
    Ok(n)
}

fn nested_labels(rows: Vec<Vec<u32>>, stop: bool) -> Result<u32, AppError> {
    'outer: for row in rows.iter() {
        'inner: for cell in row.iter() {
            if stop {
                break 'outer;
            }
            if *cell == 0 {
                continue 'outer;
            }
            fetch(*cell).map_err(AppError::Fetch)?;
        }
        store(1).map_err(AppError::Store)?;
    }
    Ok(1)
}

fn literals(name: &str) -> Result<u32, AppError> {
    let a = fetch(1).context("a . b")?;
    if name == "x ( y" {
        store(a).map_err(AppError::Store)?;
    }
    Ok(a)
}

fn labelled_blocks(stop: bool) -> Result<u32, AppError> {
    'outer: {
        fetch(0).map_err(AppError::Fetch)?;
        'inner: {
            store(1).map_err(AppError::Store)?;
            if stop {
                break 'outer;
            }
        }
        fetch(2).map_err(AppError::Fetch)?;
    }
    Ok(1)
}

fn char_literals(c: char, b: u8) -> Result<u32, AppError> {
    if c == 'a' {
        fetch(1).map_err(AppError::Fetch)?;
    }
    if b == b'x' {
        store(1).map_err(AppError::Store)?;
    }
    Ok(1)
}

fn one_element_tuples(id: u32) -> Result<u32, AppError> {
    if check((id,)) {
        fetch(id).map_err(AppError::Fetch)?;
    }
    Ok(id)
}
