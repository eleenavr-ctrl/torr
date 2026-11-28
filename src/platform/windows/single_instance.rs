use crate::platform::SingleInstance;
use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions, Stream};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::thread;

pub struct WindowsSingleInstance {
    #[cfg(windows)]
    mutex_handle: isize,
    socket_name: String,
}

impl WindowsSingleInstance {
    pub fn acquire<F>(data_dir: &Path, on_show: F) -> Option<Self>
    where
        F: Fn() + Send + Sync + 'static,
    {
        let hash = path_hash(data_dir);
        let socket_name = format!("torrserverui_{hash:016x}.sock");

        #[cfg(windows)]
        let mutex_handle = {
            use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
            use windows_sys::Win32::System::Threading::CreateMutexW;

            let mutex_name: Vec<u16> = format!("Local\\TorrServerUI_{hash:016x}\0")
                .encode_utf16()
                .collect();
            unsafe {
                let handle = CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr());
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    if !handle.is_null() {
                        CloseHandle(handle);
                    }
                    send_show_signal(&socket_name);
                    return None;
                }
                handle as isize
            }
        };

        start_ipc_listener(&socket_name, on_show);

        Some(Self {
            #[cfg(windows)]
            mutex_handle,
            socket_name,
        })
    }
}

impl SingleInstance for WindowsSingleInstance {
    fn release(&mut self) {
        #[cfg(windows)]
        if self.mutex_handle != 0 {
            use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
            unsafe {
                CloseHandle(self.mutex_handle as HANDLE);
            }
            self.mutex_handle = 0;
        }
        let _ = &self.socket_name;
    }
}

impl Drop for WindowsSingleInstance {
    fn drop(&mut self) {
        self.release();
    }
}

fn path_hash(path: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.to_string_lossy().to_lowercase().hash(&mut hasher);
    hasher.finish()
}

fn send_show_signal(socket_name: &str) {
    if let Ok(name) = socket_name.to_ns_name::<GenericNamespaced>() {
        if let Ok(mut stream) = Stream::connect(name) {
            let _ = stream.write_all(b"SHOW\n");
            let _ = stream.flush();
        }
    }
}

fn start_ipc_listener<F>(socket_name: &str, on_show: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let sock_name = socket_name.to_string();
    let _ = thread::Builder::new()
        .name("single-instance-ipc".to_string())
        .spawn(move || {
            let Ok(name) = sock_name.to_ns_name::<GenericNamespaced>() else {
                return;
            };
            let Ok(listener) = ListenerOptions::new().name(name).create_sync() else {
                return;
            };
            for conn in listener.incoming().flatten() {
                let mut reader = BufReader::new(conn);
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() && line.trim() == "SHOW" {
                    on_show();
                }
            }
        });
}
