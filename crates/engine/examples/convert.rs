use simeji_engine::{Engine, EnginePaths};
use std::{error::Error, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let usage = "Usage: convert <dictionary-directory> <scratch-directory> <reading>";
    let dictionary = PathBuf::from(args.next().ok_or(usage)?);
    let scratch = PathBuf::from(args.next().ok_or(usage)?);
    let reading = args
        .next()
        .ok_or(usage)?
        .into_string()
        .map_err(|_| "The reading must be Unicode")?;
    let mut engine = Engine::open(EnginePaths {
        dictionary: &dictionary,
        learning: &scratch.join("Learning"),
        user_dictionary: &scratch.join("user-dictionary.json"),
    })?;
    for candidate in engine.clause_candidates(&reading, "")? {
        println!("{}\t{}", candidate.text, candidate.reading);
    }
    Ok(())
}
