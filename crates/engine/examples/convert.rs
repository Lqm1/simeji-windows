use simeji_engine::Engine;
use std::{error::Error, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let Some(dict) = args.next() else {
        return Err(
            "Usage: simeji-engine <dictionary-directory> <learning-directory> <reading>".into(),
        );
    };
    let learn = PathBuf::from(args.next().ok_or("Missing learning directory")?);
    let reading = args
        .next()
        .ok_or("Missing reading")?
        .into_string()
        .map_err(|_| "Reading must be Unicode")?;
    let mut engine = Engine::open(&PathBuf::from(dict), &learn)?;
    for candidate in engine.clause_candidates(&reading, "")? {
        println!("{}\t{}", candidate.text, candidate.reading);
    }
    Ok(())
}
