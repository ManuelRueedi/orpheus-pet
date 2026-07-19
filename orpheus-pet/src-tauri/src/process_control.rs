use std::path::Path;

#[cfg(not(windows))]
use std::process::Child;

fn normalized_executable(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/").to_lowercase();
    normalized
        .strip_prefix("//?/")
        .unwrap_or(&normalized)
        .trim_end_matches('/')
        .to_string()
}

fn executable_name(path: &Path) -> String {
    path.to_string_lossy()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_string()
}

fn should_recover_process(
    actual: &Path,
    expected: &[&Path],
    parent: Option<&Path>,
    app_executable: &Path,
) -> bool {
    let actual = normalized_executable(actual);
    let is_managed = expected
        .iter()
        .any(|path| normalized_executable(path) == actual);
    if !is_managed {
        return false;
    }

    // A second live Orpheus instance must not take over the first one's stack.
    // A missing/different parent means the managed child survived a crash,
    // forced installer shutdown, or development rebuild and is safe to reclaim.
    !parent.is_some_and(|path| {
        executable_name(path).eq_ignore_ascii_case(&executable_name(app_executable))
    })
}

#[cfg(windows)]
mod platform {
    use super::should_recover_process;
    use std::{
        ffi::c_void,
        io,
        mem::size_of,
        os::windows::io::AsRawHandle,
        path::{Path, PathBuf},
        process::Child,
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Threading::{
                OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
            },
        },
    };

    pub struct ProcessContainment {
        handle: Option<usize>,
        setup_error: Option<String>,
    }

    impl Default for ProcessContainment {
        fn default() -> Self {
            match Self::create() {
                Ok(handle) => Self {
                    handle: Some(handle as usize),
                    setup_error: None,
                },
                Err(error) => Self {
                    handle: None,
                    setup_error: Some(error),
                },
            }
        }
    }

    impl ProcessContainment {
        fn create() -> Result<HANDLE, String> {
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(format!(
                    "could not create the child-process job: {}",
                    io::Error::last_os_error()
                ));
            }

            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if configured == 0 {
                let error = io::Error::last_os_error();
                unsafe { CloseHandle(handle) };
                return Err(format!(
                    "could not configure the child-process job: {error}"
                ));
            }
            Ok(handle)
        }

        pub fn assign(&self, child: &Child) -> Result<(), String> {
            let Some(handle) = self.handle else {
                return Err(self
                    .setup_error
                    .clone()
                    .unwrap_or_else(|| "child-process containment is unavailable".to_string()));
            };
            let assigned = unsafe {
                AssignProcessToJobObject(handle as HANDLE, child.as_raw_handle() as HANDLE)
            };
            if assigned == 0 {
                return Err(format!(
                    "could not contain child process {}: {}",
                    child.id(),
                    io::Error::last_os_error()
                ));
            }
            Ok(())
        }
    }

    impl Drop for ProcessContainment {
        fn drop(&mut self) {
            if let Some(handle) = self.handle.take() {
                // KILL_ON_JOB_CLOSE makes forced app termination clean up every
                // speech child even when Rust's normal exit callback cannot run.
                unsafe { CloseHandle(handle as HANDLE) };
            }
        }
    }

    fn process_image_path(pid: u32) -> Option<PathBuf> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return None;
        }
        let mut buffer = vec![0u16; 32_768];
        let mut length = buffer.len() as u32;
        let queried =
            unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) };
        unsafe { CloseHandle(handle) };
        (queried != 0).then(|| PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize])))
    }

    fn terminate_verified_process(pid: u32) -> Result<(), String> {
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            return Err(format!(
                "could not open stale process {pid}: {}",
                io::Error::last_os_error()
            ));
        }
        let terminated = unsafe { TerminateProcess(handle, 0) };
        if terminated != 0 {
            unsafe { WaitForSingleObject(handle, 5_000) };
        }
        let error = (terminated == 0).then(io::Error::last_os_error);
        unsafe { CloseHandle(handle) };
        match error {
            Some(error) => Err(format!("could not stop stale process {pid}: {error}")),
            None => Ok(()),
        }
    }

    pub fn recover_orphaned_managed_processes(expected: &[&Path]) -> Vec<String> {
        let mut notes = Vec::new();
        let app_executable = match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                notes.push(format!("stale process recovery unavailable: {error}"));
                return notes;
            }
        };
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            notes.push(format!(
                "stale process recovery unavailable: {}",
                io::Error::last_os_error()
            ));
            return notes;
        }

        let mut entry = PROCESSENTRY32W::default();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while has_entry {
            let pid = entry.th32ProcessID;
            if pid != std::process::id() {
                if let Some(actual) = process_image_path(pid) {
                    let parent = process_image_path(entry.th32ParentProcessID);
                    if should_recover_process(&actual, expected, parent.as_deref(), &app_executable)
                    {
                        match terminate_verified_process(pid) {
                            Ok(()) => notes.push(format!(
                                "recovered stale managed process {} (pid {pid})",
                                actual.display()
                            )),
                            Err(error) => notes.push(error),
                        }
                    }
                }
            }
            has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        unsafe { CloseHandle(snapshot) };
        notes
    }
}

#[cfg(windows)]
pub use platform::{recover_orphaned_managed_processes, ProcessContainment};

#[cfg(not(windows))]
#[derive(Default)]
pub struct ProcessContainment;

#[cfg(not(windows))]
impl ProcessContainment {
    pub fn assign(&self, _child: &Child) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(not(windows))]
pub fn recover_orphaned_managed_processes(_expected: &[&Path]) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_orphaned_exact_managed_executables_are_recovered() {
        let expected = [Path::new("C:/app/runtime/llama-server.exe")];
        let app = Path::new("C:/Program Files/Orpheus Pet/orpheus-pet.exe");

        assert!(should_recover_process(
            Path::new("c:\\APP\\runtime\\llama-server.exe"),
            &expected,
            None,
            app,
        ));
        assert!(should_recover_process(
            expected[0],
            &expected,
            Some(Path::new("C:/Windows/explorer.exe")),
            app,
        ));
        assert!(!should_recover_process(
            expected[0],
            &expected,
            Some(Path::new("D:/old install/ORPHEUS-PET.EXE")),
            app,
        ));
        assert!(!should_recover_process(
            Path::new("C:/other/llama-server.exe"),
            &expected,
            None,
            app,
        ));
    }

    #[cfg(windows)]
    #[test]
    fn closing_the_job_stops_an_assigned_child() {
        use std::{
            process::{Command, Stdio},
            thread,
            time::{Duration, Instant},
        };

        let mut child = Command::new("cmd.exe")
            .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("test child should start");
        let containment = ProcessContainment::default();
        containment
            .assign(&child)
            .expect("test child should enter the kill-on-close job");

        drop(containment);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if child
                .try_wait()
                .expect("test child should remain observable")
                .is_some()
            {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        panic!("assigned child survived after the job handle closed");
    }
}
