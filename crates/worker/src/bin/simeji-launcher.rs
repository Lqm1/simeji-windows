//! Starts the user's conversion worker unless it is already running.
#![windows_subsystem = "windows"]
use simeji_protocol::{Request, Response, WORKER_PIPE};
use std::{
    error::Error,
    os::windows::process::CommandExt,
    process::{Command, Stdio},
    time::Duration,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PING_TIMEOUT: Duration = Duration::from_millis(250);

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    if matches!(
        simeji_transport::call(WORKER_PIPE, &Request::Ping, PING_TIMEOUT),
        Ok(Response::Ready)
    ) {
        return Ok(());
    }
    let executable = std::env::current_exe()?;
    let directory = executable.parent().ok_or("Missing executable directory")?;
    Command::new(directory.join("simeji-worker.exe"))
        .arg("serve")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}
