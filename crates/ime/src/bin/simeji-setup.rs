use std::{
    error::Error,
    path::{Path, PathBuf},
};
use windows::{
    Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW},
    core::{HSTRING, PCWSTR},
};

const USAGE: &str =
    "Usage: simeji-setup register <dll-path> | verify <dll-path> | unregister | remove <path>";
const MOVED_ASIDE: &str = "removed";

fn remove(path: &Path) -> Result<(), Box<dyn Error>> {
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            remove(&entry?.path())?;
        }
        if std::fs::remove_dir(path).is_err() {
            remove_at_restart(path)?;
        }
    } else if std::fs::remove_file(path).is_err() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let target = match aside_name(path, unique) {
            Some(moved) => {
                std::fs::rename(path, &moved)?;
                moved
            }
            None => path.to_owned(),
        };
        remove_at_restart(&target)?;
    }
    Ok(())
}

/// A deletion scheduled for the name itself would delete a file that a later
/// installation puts there, so a file in use moves aside first, once.
fn aside_name(path: &Path, unique: u128) -> Option<PathBuf> {
    if path
        .extension()
        .is_some_and(|extension| extension == MOVED_ASIDE)
    {
        return None;
    }
    let mut name = path.file_name()?.to_owned();
    name.push(format!(".{unique}.{MOVED_ASIDE}"));
    Some(path.with_file_name(name))
}

fn remove_at_restart(path: &Path) -> windows::core::Result<()> {
    unsafe {
        MoveFileExW(
            &HSTRING::from(path),
            PCWSTR::null(),
            MOVEFILE_DELAY_UNTIL_REBOOT,
        )?;
    }
    println!("Removed when Windows restarts: {}", path.display());
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref().and_then(|s| s.to_str()) {
        Some(command @ ("register" | "verify")) => {
            let path = PathBuf::from(args.next().ok_or("Missing DLL path")?).canonicalize()?;
            if command == "verify" {
                simeji_ime::registration::verify(&path)?;
            } else {
                simeji_ime::registration::verify(&path)?;
                simeji_ime::registration::register(&path)?;
            }
        }
        Some("unregister") => simeji_ime::registration::unregister()?,
        Some("remove") => {
            let path = PathBuf::from(args.next().ok_or("Missing path")?);
            if path.exists() {
                remove(&path)?;
            }
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_a_directory_tree() -> Result<(), Box<dyn Error>> {
        let root = std::env::temp_dir().join(format!("simeji-setup-remove-{}", std::process::id()));
        std::fs::create_dir_all(root.join("Versions").join("old"))?;
        std::fs::write(
            root.join("Versions").join("old").join("simeji_ime.dll"),
            b"dll",
        )?;
        std::fs::write(root.join("README.md"), b"readme")?;
        remove(&root)?;
        assert!(!root.exists());
        Ok(())
    }

    #[test]
    fn a_file_in_use_moves_aside_only_once() {
        let dll = Path::new(r"C:\Program Files\SimejiWindows\simeji_ime.dll");
        let moved = aside_name(dll, 7).unwrap();
        assert_eq!(
            moved,
            Path::new(r"C:\Program Files\SimejiWindows\simeji_ime.dll.7.removed")
        );
        assert_eq!(aside_name(&moved, 8), None);
    }
}
