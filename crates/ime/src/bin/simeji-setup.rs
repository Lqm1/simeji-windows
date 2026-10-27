use std::{error::Error, path::PathBuf};
fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref().and_then(|s| s.to_str()) {
        Some("register") => {
            let path = PathBuf::from(args.next().ok_or("Missing DLL path")?).canonicalize()?;
            simeji_ime::registration::register(&path)?;
        }
        Some("unregister") => simeji_ime::registration::unregister()?,
        _ => return Err("Usage: simeji-setup register <dll-path> | unregister".into()),
    }
    Ok(())
}
