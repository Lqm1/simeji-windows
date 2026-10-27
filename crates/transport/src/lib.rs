//! Local, owner-only named pipes carrying one request and one response per
//! connection. Overlapped IO bounds every wait.
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::File,
    hash::{DefaultHasher, Hash, Hasher},
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
    core::{PCWSTR, w},
};

pub const MAX_FRAME: usize = 1024 * 1024;
const DEFAULT_CHANNEL: &str = "engine-v1";
const MAX_CHANNEL_LENGTH: usize = 64;
/// Sent by the client after it read the response, so the server keeps the
/// connection open until the whole response was consumed.
const ACKNOWLEDGEMENT: u8 = 0xa5;
const SECURITY_DESCRIPTOR: PCWSTR = w!("D:P(A;;GA;;;OW)");
const BUSY_RETRY: Duration = Duration::from_millis(1);
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn pipe_name(channel: &str) -> Result<Vec<u16>> {
    if channel.len() > MAX_CHANNEL_LENGTH
        || !channel
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Invalid pipe channel").into());
    }
    let mut hash = DefaultHasher::new();
    std::env::var_os("USERPROFILE").hash(&mut hash);
    Ok(format!(
        "\\\\.\\pipe\\simeji-windows-{:016x}-{channel}\0",
        hash.finish()
    )
    .encode_utf16()
    .collect())
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
    pub fn server() -> Result<Self> {
        Self::server_channel(DEFAULT_CHANNEL)
    }
    pub fn server_channel(channel: &str) -> Result<Self> {
        let name = pipe_name(channel)?;
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                SECURITY_DESCRIPTOR,
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )?;
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            };
            let handle = CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                MAX_FRAME as u32,
                MAX_FRAME as u32,
                0,
                Some(&attributes),
            );
            let error = if handle == INVALID_HANDLE_VALUE {
                Some(windows::core::Error::from_thread())
            } else {
                None
            };
            let _ = LocalFree(Some(HLOCAL(descriptor.0)));
            if let Some(error) = error {
                return Err(error.into());
            }
            Ok(Self {
                file: File::from_raw_handle(handle.0),
            })
        }
    }
    pub fn connect(timeout: Duration) -> Result<Self> {
        Self::connect_channel(DEFAULT_CHANNEL, timeout)
    }
    pub fn connect_channel(channel: &str, timeout: Duration) -> Result<Self> {
        let name = pipe_name(channel)?;
        let end = Instant::now() + timeout;
        loop {
            unsafe {
                match CreateFileW(
                    PCWSTR(name.as_ptr()),
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    None,
                ) {
                    Ok(handle) => {
                        return Ok(Self {
                            file: File::from_raw_handle(handle.0),
                        });
                    }
                    Err(error) => {
                        if error.code() != ERROR_PIPE_BUSY.to_hresult() || Instant::now() >= end {
                            return Err(error.into());
                        }
                    }
                }
            }
            std::thread::sleep(BUSY_RETRY);
        }
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
                // Drain cancellation before dropping the OVERLAPPED and buffer.
                let _ = GetOverlappedResult(self.handle(), &overlapped, &mut transferred, true);
                return Err(
                    io::Error::new(io::ErrorKind::TimedOut, "Pipe operation timed out").into(),
                );
            }
            GetOverlappedResult(self.handle(), &overlapped, &mut transferred, false)?;
            Ok(transferred)
        }
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
    fn read_exact(&self, buffer: &mut [u8], deadline: Instant) -> Result<()> {
        let mut offset = 0;
        while offset < buffer.len() {
            let n = self.overlapped(Some(deadline), |overlapped| unsafe {
                ReadFile(
                    self.handle(),
                    Some(&mut buffer[offset..]),
                    None,
                    Some(overlapped),
                )
            })? as usize;
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
            }
            offset += n;
        }
        Ok(())
    }
    fn write_all(&self, buffer: &[u8], deadline: Instant) -> Result<()> {
        let mut offset = 0;
        while offset < buffer.len() {
            let n = self.overlapped(Some(deadline), |overlapped| unsafe {
                WriteFile(
                    self.handle(),
                    Some(&buffer[offset..]),
                    None,
                    Some(overlapped),
                )
            })? as usize;
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::WriteZero).into());
            }
            offset += n;
        }
        Ok(())
    }
    pub fn receive<T: DeserializeOwned>(&self, timeout: Duration) -> Result<T> {
        let deadline = Instant::now() + timeout;
        let mut header = [0; 4];
        self.read_exact(&mut header, deadline)?;
        let length = u32::from_le_bytes(header) as usize;
        if length > MAX_FRAME {
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "Pipe frame is too large").into(),
            );
        }
        let mut bytes = vec![0; length];
        self.read_exact(&mut bytes, deadline)?;
        Ok(serde_json::from_slice(&bytes)?)
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
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "Invalid acknowledgement").into(),
            );
        }
        Ok(())
    }
    pub fn send<T: Serialize>(&self, value: &T, timeout: Duration) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > MAX_FRAME {
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "Pipe frame is too large").into(),
            );
        }
        let deadline = Instant::now() + timeout;
        self.write_all(&(bytes.len() as u32).to_le_bytes(), deadline)?;
        self.write_all(&bytes, deadline)
    }
}

pub fn exchange<Q: Serialize, R: DeserializeOwned>(request: &Q, timeout: Duration) -> Result<R> {
    exchange_channel(DEFAULT_CHANNEL, request, timeout)
}
pub fn exchange_channel<Q: Serialize, R: DeserializeOwned>(
    channel: &str,
    request: &Q,
    timeout: Duration,
) -> Result<R> {
    let pipe = Pipe::connect_channel(channel, timeout)?;
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
        let channel = format!("test-{}", std::process::id());
        let server = Pipe::server_channel(&channel).unwrap();
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
                exchange_channel::<_, String>(&channel, &"にほんご", Duration::from_secs(1))
                    .unwrap(),
                "日本語".repeat(10_000)
            );
        }
        let client = Pipe::connect_channel(&channel, Duration::from_secs(1)).unwrap();
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
}
