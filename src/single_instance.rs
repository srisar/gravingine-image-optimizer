//! One window at a time. The first copy of the app owns a named mutex and listens on a named
//! pipe. A later copy (Explorer's "Optimize with Gravingine" starts one per selected file;
//! dropping files on the .exe or starting it from the Start menu starts one too) hands its
//! paths to the first one, lets it come to the front, and exits.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use winapi::shared::minwindef::{DWORD, FALSE};
use winapi::shared::winerror::{ERROR_ALREADY_EXISTS, ERROR_NO_DATA, ERROR_PIPE_CONNECTED};
use winapi::um::errhandlingapi::GetLastError;
use winapi::um::fileapi::ReadFile;
use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
use winapi::um::namedpipeapi::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe};
use winapi::um::processthreadsapi::{GetCurrentProcessId, ProcessIdToSessionId};
use winapi::um::synchapi::CreateMutexW;
use winapi::um::winbase::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use winapi::um::winnt::HANDLE;
use winapi::um::winuser::{ASFW_ANY, AllowSetForegroundWindow};

/// Starts every message, so stray connections are ignored.
const MAGIC: &[u8] = b"GIO1";
/// Largest message read (a few thousand paths).
const MAX_MESSAGE: usize = 4 * 1024 * 1024;
/// How long a later copy keeps trying to reach the first (which may still be starting).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The pipe's name: one per Windows session, like the `Local\` mutex.
fn pipe_name() -> String {
    let mut session: DWORD = 0;
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    format!(r"\\.\pipe\GravingineImageOptimizer-{session}")
}

/// The pipe handle, moved to the listening thread.
struct Pipe(HANDLE);
unsafe impl Send for Pipe {}

/// This copy is the first: call [`Listener::start`] once the app can take paths.
pub struct Listener {
    pipe: Pipe,
}

/// Claim being the only copy. Returns `None` when another copy is running and has been handed
/// `paths` (made absolute): this copy should exit. Otherwise this copy carries on, and the
/// returned listener (if the pipe could be created) receives later copies' paths.
pub fn claim(paths: &[PathBuf]) -> Option<Option<Listener>> {
    // Kept for the life of the process: Windows releases it when the process ends.
    let mutex = unsafe { CreateMutexW(std::ptr::null_mut(), FALSE, wide(r"Local\GravingineImageOptimizer.Instance").as_ptr()) };
    let already_running = !mutex.is_null() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already_running {
        let absolute: Vec<PathBuf> = paths.iter().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone())).collect();
        if forward(&absolute) {
            return None;
        }
        // The first copy didn't answer (stuck or closing): open a window here after all.
        return Some(None);
    }
    Some(create_pipe(true).map(|pipe| Listener { pipe }))
}

impl Listener {
    /// Listen on a background thread, calling `on_paths` with each later copy's paths (empty
    /// when it was started with none: just come to the front).
    pub fn start(self, on_paths: impl Fn(Vec<PathBuf>) + Send + 'static) {
        let mut pipe = self.pipe;
        std::thread::spawn(move || {
            loop {
                // A client may have connected before this call (PIPE_CONNECTED), or even
                // written its paths and closed its end already (NO_DATA): its message is still
                // in the pipe either way.
                let connected = unsafe { ConnectNamedPipe(pipe.0, std::ptr::null_mut()) } != 0
                    || matches!(unsafe { GetLastError() }, ERROR_PIPE_CONNECTED | ERROR_NO_DATA);
                if connected && let Some(paths) = decode(&read_all(&pipe)) {
                    on_paths(paths);
                }
                unsafe { DisconnectNamedPipe(pipe.0) };
                if !connected {
                    // Something is wrong with this instance of the pipe: make a new one.
                    unsafe { CloseHandle(pipe.0) };
                    std::thread::sleep(Duration::from_millis(200));
                    match create_pipe(false) {
                        Some(p) => pipe = p,
                        None => return,
                    }
                }
            }
        });
    }
}

/// The first copy's end of the pipe: inbound only, local clients only. The first instance is
/// created with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so a pipe someone else made first is refused.
fn create_pipe(first: bool) -> Option<Pipe> {
    let open_mode = PIPE_ACCESS_INBOUND | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
    let handle = unsafe {
        CreateNamedPipeW(
            wide(&pipe_name()).as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            0,
            64 * 1024,
            0,
            std::ptr::null_mut(),
        )
    };
    (handle != INVALID_HANDLE_VALUE).then_some(Pipe(handle))
}

/// Everything a client writes, until it closes its end (capped at `MAX_MESSAGE`).
fn read_all(pipe: &Pipe) -> Vec<u8> {
    let mut data = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let mut read: DWORD = 0;
        let ok = unsafe { ReadFile(pipe.0, buf.as_mut_ptr().cast(), buf.len() as DWORD, &mut read, std::ptr::null_mut()) };
        if ok == 0 || read == 0 {
            return data;
        }
        data.extend_from_slice(&buf[..read as usize]);
        if data.len() > MAX_MESSAGE {
            return Vec::new();
        }
    }
}

/// Hand `paths` to the running copy, letting it come to the front. Retries while it's busy
/// with another copy or still starting.
fn forward(paths: &[PathBuf]) -> bool {
    use std::io::Write;
    let name = pipe_name();
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match std::fs::OpenOptions::new().write(true).open(&name) {
            Ok(mut pipe) => {
                // This copy was just started by the user, so it may pass on the right to
                // take the foreground.
                unsafe { AllowSetForegroundWindow(ASFW_ANY) };
                return pipe.write_all(&encode(paths)).is_ok();
            }
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(40)),
            Err(_) => return false,
        }
    }
}

/// `MAGIC`, then each path as UTF-16 (little-endian) followed by a 0 unit.
fn encode(paths: &[PathBuf]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    for path in paths {
        for unit in path.as_os_str().encode_wide().chain(Some(0)) {
            out.extend_from_slice(&unit.to_le_bytes());
        }
    }
    out
}

/// The paths in a message, or `None` if it isn't one.
fn decode(data: &[u8]) -> Option<Vec<PathBuf>> {
    let body = data.strip_prefix(MAGIC)?;
    if body.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = body.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect();
    let paths = units
        .split(|&u| u == 0)
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(OsString::from_wide(s)))
        .collect();
    Some(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_paths() {
        let paths = vec![
            PathBuf::from(r"D:\Photos\Holiday 2026\IMG_0001.JPG"),
            PathBuf::from(r"C:\Users\Ünïcødé\写真\cat.webp"),
            PathBuf::from(r"\\server\share\folder"),
        ];
        assert_eq!(decode(&encode(&paths)), Some(paths));
        // No paths: "come to the front".
        assert_eq!(decode(&encode(&[])), Some(Vec::new()));
    }

    #[test]
    fn stray_messages_are_ignored() {
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"hello"), None);
        assert_eq!(decode(b"GIO1\x41"), None); // odd length
    }
}
