#![windows_subsystem = "windows"]
use simeji_protocol::{Request, Response};
use std::{
    error::Error,
    os::windows::process::CommandExt,
    process::{Command, Stdio},
    time::Duration,
};

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    if matches!(
        simeji_transport::exchange::<_, Response>(&Request::Ping, Duration::from_millis(250)),
        Ok(Response::Ready)
    ) {
        return Ok(());
    }
    let executable = std::env::current_exe()?;
    let directory = executable.parent().ok_or("Missing executable directory")?;
    let data =
        std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("Missing LOCALAPPDATA")?)
            .join("SimejiWindows")
            .join("UserData");
    std::fs::create_dir_all(&data)?;
    Command::new(directory.join("simeji-worker.exe"))
        .arg("serve")
        .arg(directory.join("dict"))
        .arg(data)
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}
