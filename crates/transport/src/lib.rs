//! Named pipes of the signed-in user's logon session, carrying one request
//! and one response per connection. Overlapped IO bounds every wait.
//!
//! A pipe is named after the user's SID and session, owned by the user and
//! open to the user's processes, including AppContainer and low integrity
//! ones. Clients only talk to a pipe that the user owns.

mod identity;

use identity::User;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::File,
    io,
    os::windows::io::{AsRawHandle, FromRawHandle},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::*,
        Security::{Authorization::*, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES},
        Storage::FileSystem::*,
        System::{IO::*, Pipes::*, Threading::*},
    },
    core::HSTRING,
};

pub const MAX_FRAME: usize = 1024 * 1024;
const MAX_NAME_LENGTH: usize = 64;
/// Sent by the client after it read the response, so the server keeps the
/// connection open until the whole response was consumed.
const ACKNOWLEDGEMENT: u8 = 0xa5;
const BUSY_RETRY: Duration = Duration::from_millis(1);

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn path(name: &str) -> Result<HSTRING> {
    if name.is_empty()
        || name.len() > MAX_NAME_LENGTH
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Invalid pipe name").into());
    }
    let user = User::current()?;
    Ok(HSTRING::from(format!(
        r"\\.\pipe\SimejiWindows\{}\{}\{name}",
        user.sid_string, user.session
    )))
}

/// Owner and full access for the user; read and write for the user's
/// AppContainer processes; writable from low integrity.
fn security_descriptor() -> Result<HSTRING> {
    let sid = &User::current()?.sid_string;
    Ok(HSTRING::from(format!(
        "O:{sid}D:P(A;;GA;;;{sid})(A;;GRGW;;;AC)S:(ML;;NW;;;LW)"
    )))
}

fn io_error(kind: io::ErrorKind, message: &str) -> Box<dyn std::error::Error + Send + Sync> {
    io::Error::new(kind, message).into()
}

struct OwnedEvent(HANDLE);

impl Drop for OwnedEvent {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub struct Pipe {
    file: File,
}

impl Pipe {
    /// Creates the only instance of the pipe; fails if it already exists.
    pub fn listen(name: &str) -> Result<Self> {
        let path = path(name)?;
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &security_descriptor()?,
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )?;
            let attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            };
            let handle = CreateNamedPipeW(
                &path,
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                MAX_FRAME as u32,
                MAX_FRAME as u32,
                0,
                Some(&attributes),
            );
            let error = (handle == INVALID_HANDLE_VALUE).then(windows::core::Error::from_thread);
            let _ = LocalFree(Some(HLOCAL(descriptor.0)));
            if let Some(error) = error {
                return Err(error.into());
            }
            Ok(Self {
                file: File::from_raw_handle(handle.0),
            })
        }
    }

    /// Opens the pipe, waiting while another client uses it. The server may
    /// identify but not impersonate the client.
    pub fn connect(name: &str, timeout: Duration) -> Result<Self> {
        let path = path(name)?;
        let end = Instant::now() + timeout;
        let handle = loop {
            let opened = unsafe {
                CreateFileW(
                    &path,
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    None,
                )
            };
            match opened {
                Ok(handle) => break handle,
                Err(error)
                    if error.code() == ERROR_PIPE_BUSY.to_hresult() && Instant::now() < end =>
                {
                    std::thread::sleep(BUSY_RETRY);
                }
                Err(error) => return Err(error.into()),
            }
        };
        let pipe = Self {
            file: unsafe { File::from_raw_handle(handle.0) },
        };
        if !User::current()?.owns(pipe.handle())? {
            return Err(io_error(
                io::ErrorKind::PermissionDenied,
                "The pipe belongs to another user",
            ));
        }
        Ok(pipe)
    }

    pub fn accept(&self) -> Result<()> {
        self.overlapped(None, |overlapped| unsafe {
            ConnectNamedPipe(self.handle(), Some(overlapped))
        })
        .map(|_| ())
    }

    pub fn disconnect(&self) -> Result<()> {
        unsafe {
            DisconnectNamedPipe(self.handle())?;
        }
        Ok(())
    }

    /// Serves one connection: receives a request, sends `respond(request)`
    /// and waits for the client to acknowledge it.
    pub fn answer<Q: DeserializeOwned, R: Serialize>(
        &self,
        timeout: Duration,
        respond: impl FnOnce(Q) -> R,
    ) -> Result<()> {
        let request = self.receive::<Q>(timeout)?;
        self.send(&respond(request), timeout)?;
        if self.receive::<u8>(timeout)? != ACKNOWLEDGEMENT {
            return Err(io_error(
                io::ErrorKind::InvalidData,
                "Invalid acknowledgement",
            ));
        }
        Ok(())
    }

    pub fn receive<T: DeserializeOwned>(&self, timeout: Duration) -> Result<T> {
        let deadline = Instant::now() + timeout;
        let mut header = [0; 4];
        self.read_exact(&mut header, deadline)?;
        let length = u32::from_le_bytes(header) as usize;
        if length > MAX_FRAME {
            return Err(io_error(
                io::ErrorKind::InvalidData,
                "Pipe frame is too large",
            ));
        }
        let mut bytes = vec![0; length];
        self.read_exact(&mut bytes, deadline)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn send<T: Serialize>(&self, value: &T, timeout: Duration) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > MAX_FRAME {
            return Err(io_error(
                io::ErrorKind::InvalidData,
                "Pipe frame is too large",
            ));
        }
        let deadline = Instant::now() + timeout;
        self.write_all(&(bytes.len() as u32).to_le_bytes(), deadline)?;
        self.write_all(&bytes, deadline)
    }

    fn handle(&self) -> HANDLE {
        HANDLE(self.file.as_raw_handle())
    }

    fn overlapped(
        &self,
        deadline: Option<Instant>,
        start: impl FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>,
    ) -> Result<u32> {
        unsafe {
            let event = OwnedEvent(CreateEventW(None, true, false, None)?);
            let mut overlapped = OVERLAPPED {
                hEvent: event.0,
                ..Default::default()
            };
            match start(&mut overlapped) {
                Ok(()) => {}
                Err(error) if error.code() == ERROR_IO_PENDING.to_hresult() => {}
                Err(error) if error.code() == ERROR_PIPE_CONNECTED.to_hresult() => return Ok(0),
                Err(error) => return Err(error.into()),
            }
            let milliseconds = deadline.map_or(INFINITE, |end| {
                end.saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(u128::from(u32::MAX - 1)) as u32
            });
            let waited = WaitForSingleObject(event.0, milliseconds);
            let mut transferred = 0;
            if waited != WAIT_OBJECT_0 {
                let _ = CancelIoEx(self.handle(), Some(&overlapped));
                // The cancelled operation still writes `overlapped` and the buffer.
                let _ = GetOverlappedResult(self.handle(), &overlapped, &mut transferred, true);
                return Err(io_error(
                    io::ErrorKind::TimedOut,
                    "Pipe operation timed out",
                ));
            }
            GetOverlappedResult(self.handle(), &overlapped, &mut transferred, false)?;
            Ok(transferred)
        }
    }

    fn read_exact(&self, buffer: &mut [u8], deadline: Instant) -> Result<()> {
        let mut offset = 0;
        while offset < buffer.len() {
            let read = self.overlapped(Some(deadline), |overlapped| unsafe {
                ReadFile(
                    self.handle(),
                    Some(&mut buffer[offset..]),
                    None,
                    Some(overlapped),
                )
            })? as usize;
            if read == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
            }
            offset += read;
        }
        Ok(())
    }

    fn write_all(&self, buffer: &[u8], deadline: Instant) -> Result<()> {
        let mut offset = 0;
        while offset < buffer.len() {
            let written = self.overlapped(Some(deadline), |overlapped| unsafe {
                WriteFile(
                    self.handle(),
                    Some(&buffer[offset..]),
                    None,
                    Some(overlapped),
                )
            })? as usize;
            if written == 0 {
                return Err(io::Error::from(io::ErrorKind::WriteZero).into());
            }
            offset += written;
        }
        Ok(())
    }
}

/// Sends `request` over a new connection to the pipe `name` and returns the
/// response.
pub fn call<Q: Serialize, R: DeserializeOwned>(
    name: &str,
    request: &Q,
    timeout: Duration,
) -> Result<R> {
    let pipe = Pipe::connect(name, timeout)?;
    pipe.send(request, timeout)?;
    let response = pipe.receive(timeout)?;
    pipe.send(&ACKNOWLEDGEMENT, timeout)?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exchanges_frames_and_cancels_a_stalled_read() {
        let name = format!("test-{}", std::process::id());
        let server = Pipe::listen(&name).unwrap();
        assert!(Pipe::listen(&name).is_err());
        let thread = std::thread::spawn(move || {
            for _ in 0..2 {
                server.accept().unwrap();
                server
                    .answer(Duration::from_secs(1), |request: String| {
                        assert_eq!(request, "にほんご");
                        "日本語".repeat(10_000)
                    })
                    .unwrap();
                server.disconnect().unwrap();
            }
            server.accept().unwrap();
            assert_eq!(
                server.receive::<String>(Duration::from_secs(1)).unwrap(),
                "にほんご"
            );
            server.send(&"日本語", Duration::from_secs(1)).unwrap();
            std::thread::sleep(Duration::from_millis(200));
            server.disconnect().unwrap();
        });
        for _ in 0..2 {
            assert_eq!(
                call::<_, String>(&name, &"にほんご", Duration::from_secs(1)).unwrap(),
                "日本語".repeat(10_000)
            );
        }
        let client = Pipe::connect(&name, Duration::from_secs(1)).unwrap();
        client.send(&"にほんご", Duration::from_secs(1)).unwrap();
        assert_eq!(
            client.receive::<String>(Duration::from_secs(1)).unwrap(),
            "日本語"
        );
        let start = Instant::now();
        assert!(client.receive::<String>(Duration::from_millis(20)).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        thread.join().unwrap();
    }

    #[test]
    fn rejects_names_outside_the_pipe_namespace() {
        for name in ["", r"..\other", "a/b", &"x".repeat(MAX_NAME_LENGTH + 1)] {
            assert!(path(name).is_err());
        }
    }
}
