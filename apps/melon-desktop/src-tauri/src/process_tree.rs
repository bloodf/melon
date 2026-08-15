//! Current-session ownership of one spawned child process tree.

use std::io;
use std::process::{Child, Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

/// Live OS handles proving ownership of one tree created by [`ProcessTree::spawn`].
///
/// No constructor accepts a PID: a discovered or persisted process can never become owned.
#[derive(Debug)]
pub(crate) struct ProcessTree {
    child: Child,
    platform: PlatformTree,
    stopped: bool,
}

#[cfg(unix)]
#[derive(Debug)]
struct PlatformTree {
    process_group: libc::pid_t,
    absence_observed: bool,
}

#[cfg(windows)]
#[derive(Debug)]
struct PlatformTree {
    job: std::os::windows::io::OwnedHandle,
}
const FALLBACK_CLEANUP: Duration = Duration::from_secs(1);

fn kill_and_reap_bounded_with<Q, K>(
    child: &mut Child,
    deadline: Instant,
    first_error: &mut Option<io::Error>,
    mut query: Q,
    kill: K,
) -> bool
where
    Q: FnMut(&mut Child) -> io::Result<bool>,
    K: FnOnce(&mut Child) -> io::Result<()>,
{
    match query(child) {
        Ok(true) => return true,
        Ok(false) => {}
        Err(error) => {
            first_error.get_or_insert(error);
        }
    }
    if let Err(error) = kill(child) {
        first_error.get_or_insert(error);
    }
    loop {
        match query(child) {
            Ok(true) => return true,
            Ok(false) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
        if Instant::now() >= deadline {
            first_error.get_or_insert_with(|| {
                io::Error::new(io::ErrorKind::TimedOut, "direct child survived bounded termination")
            });
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn kill_and_reap_bounded(
    child: &mut Child,
    deadline: Instant,
    first_error: &mut Option<io::Error>,
) -> bool {
    kill_and_reap_bounded_with(
        child,
        deadline,
        first_error,
        |child| child.try_wait().map(|status| status.is_some()),
        Child::kill,
    )
}

impl ProcessTree {
    /// Spawns `command` without changing its arguments, environment, working directory, or stdio.
    ///
    /// Ownership is returned only after pre-exec session isolation on Unix or suspended Job Object
    /// assignment on Windows. Isolation or assignment failure terminates and reaps the candidate.
    ///
    /// # Errors
    /// Returns the spawn, isolation, Job Object configuration, assignment, or resume error.
    pub(crate) fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Self::spawn_with_unix_isolation(command, || {
                // SAFETY: called by Command after fork and before exec. setsid has no Rust-owned
                // memory effects and either creates a new session/group or fails the spawn.
                if unsafe { libc::setsid() } == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            })
        }
        #[cfg(windows)]
        {
            Self::spawn_with_windows_assignment(command, |job, process| {
                use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
                // SAFETY: handles remain live for this call and the suspended child has not run.
                if unsafe { AssignProcessToJobObject(job, process) } == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            })
        }
    }

    #[cfg(unix)]
    fn spawn_with_unix_isolation<F>(command: &mut Command, isolation: F) -> io::Result<Self>
    where
        F: FnMut() -> io::Result<()> + Send + Sync + 'static,
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: caller supplies a pre-exec-safe isolation function. Production passes only setsid;
        // tests return a fixed OS error without touching shared state.
        unsafe { command.pre_exec(isolation) };
        let mut child = command.spawn()?;
        let process_group = match libc::pid_t::try_from(child.id()) {
            Ok(process_group) => process_group,
            Err(_) => {
                let mut cleanup_error = None;
                kill_and_reap_bounded(&mut child, Instant::now() + FALLBACK_CLEANUP, &mut cleanup_error);
                return Err(io::Error::other("spawned child PID does not fit pid_t"));
            }
        };
        Ok(Self {
            child,
            platform: PlatformTree { process_group, absence_observed: false },
            stopped: false,
        })
    }

    #[cfg(windows)]
    fn spawn_with_windows_assignment<F>(command: &mut Command, assign: F) -> io::Result<Self>
    where
        F: FnOnce(windows_sys::Win32::Foundation::HANDLE, windows_sys::Win32::Foundation::HANDLE) -> io::Result<()>,
    {
        use std::mem::size_of;
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectExtendedLimitInformation, SetInformationJobObject,
        };
        use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

        // SAFETY: null security/name creates an unnamed Job Object. OwnedHandle closes it exactly once.
        let raw_job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw_job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a new owned, non-null handle.
        let job = unsafe { OwnedHandle::from_raw_handle(raw_job.cast()) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: job is live; pointer and byte size exactly match requested information class.
        if unsafe {
            SetInformationJobObject(
                raw_job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }

        command.creation_flags(CREATE_SUSPENDED);
        let mut child = command.spawn()?;
        let process = child.as_raw_handle().cast();
        if let Err(error) = assign(raw_job, process) {
            drop(job);
            let mut cleanup_error = Some(error);
            kill_and_reap_bounded(&mut child, Instant::now() + FALLBACK_CLEANUP, &mut cleanup_error);
            return Err(cleanup_error.expect("assignment error retained"));
        }
        if let Err(error) = resume_suspended_process(child.id()) {
            drop(job);
            let mut cleanup_error = Some(error);
            kill_and_reap_bounded(&mut child, Instant::now() + FALLBACK_CLEANUP, &mut cleanup_error);
            return Err(cleanup_error.expect("resume error retained"));
        }
        Ok(Self { child, platform: PlatformTree { job }, stopped: false })
    }

    /// Observes only the directly spawned child's exit status.
    ///
    /// Descendants may remain live after this returns `Some`; use [`Self::is_running`] for whole-tree liveness.
    ///
    /// # Errors
    /// Returns an OS error when the direct child's status cannot be queried.
    pub(crate) fn try_wait_direct(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Reports whether any member remains in the owned process tree.
    /// # Errors
    /// Returns an OS query error other than an already-absent tree.
    pub(crate) fn is_running(&mut self) -> io::Result<bool> {
        if self.stopped {
            return Ok(false);
        }
        self.platform_is_running()
    }

    /// Gracefully stops the entire Unix group, then force-stops the same owned tree after `grace`.
    ///
    /// Windows has no general graceful child-process signal, so it waits `grace` for natural direct
    /// child exit before terminating the owned Job Object. The direct child is always reaped.
    /// Repeated calls are inert.
    ///
    /// # Errors
    /// Returns the first signal, Job Object, liveness-query, or child-reap error after best-effort
    /// completion of every remaining teardown step.
    pub(crate) fn stop(&mut self, grace: Duration) -> io::Result<()> {
        if self.stopped {
            return Ok(());
        }
        match self.stop_platform(grace) {
            Ok(()) => {
                self.stopped = true;
                Ok(())
            }
            Err(first_error) => match self.platform_is_running() {
                Ok(false) => {
                    self.stopped = true;
                    Err(first_error)
                }
                Ok(true) | Err(_) => Err(first_error),
            },
        }
    }

}

#[cfg(unix)]
impl ProcessTree {
    fn platform_is_running(&mut self) -> io::Result<bool> {
        if self.platform.absence_observed {
            return Ok(false);
        }
        // SAFETY: negative id addresses only the isolated group created during spawn; signal 0 probes it.
        let result = unsafe { libc::kill(-self.platform.process_group, 0) };
        if result == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            self.platform.absence_observed = true;
            Ok(false)
        } else {
            Err(error)
        }
    }

    fn signal_group(&mut self, signal: libc::c_int) -> io::Result<()> {
        if self.platform.absence_observed {
            return Ok(());
        }
        // SAFETY: group id comes only from successful setsid-backed spawn and never from caller input.
        if unsafe { libc::kill(-self.platform.process_group, signal) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            self.platform.absence_observed = true;
            Ok(())
        } else {
            Err(error)
        }
    }

    fn wait_until(&mut self, deadline: Instant) -> io::Result<bool> {
        while Instant::now() < deadline {
            if !self.platform_is_running()? {
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(10));
        }
        self.platform_is_running().map(|running| !running)
    }


    fn stop_platform(&mut self, grace: Duration) -> io::Result<()> {
        let mut first_error = self.signal_group(libc::SIGTERM).err();
        let graceful_exit = match self.wait_until(Instant::now() + grace) {
            Ok(exited) => exited,
            Err(error) => {
                first_error.get_or_insert(error);
                false
            }
        };
        if !graceful_exit {
            if let Err(error) = self.signal_group(libc::SIGKILL) {
                first_error.get_or_insert(error);
            }
        }

        let forced_deadline = Instant::now() + grace.max(FALLBACK_CLEANUP);
        kill_and_reap_bounded(&mut self.child, forced_deadline, &mut first_error);
        match self.wait_until(forced_deadline) {
            Ok(true) => {}
            Ok(false) => {
                first_error.get_or_insert_with(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "process group survived forced termination")
                });
            }
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(windows)]
impl ProcessTree {
    fn platform_is_running(&mut self) -> io::Result<bool> {
        use std::os::windows::io::AsRawHandle;
        Self::query_job_running(self.platform.job.as_raw_handle().cast())
    }

    fn query_job_running(job: windows_sys::Win32::Foundation::HANDLE) -> io::Result<bool> {
        use std::mem::size_of;
        use windows_sys::Win32::System::JobObjects::{
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation, QueryInformationJobObject,
        };
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: caller retains job; buffer and size match requested information class.
        if unsafe {
            QueryInformationJobObject(
                job,
                JobObjectBasicAccountingInformation,
                (&raw mut accounting).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(accounting.ActiveProcesses != 0)
        }
    }

    /// Stops with injectable OS operations so query/kill failures cannot skip later cleanup.
    fn stop_with_windows_ops<J, T, Q, K>(
        &mut self,
        grace: Duration,
        mut query_job: J,
        terminate_job: T,
        mut query_child: Q,
        kill_child: K,
    ) -> io::Result<()>
    where
        J: FnMut(windows_sys::Win32::Foundation::HANDLE) -> io::Result<bool>,
        T: FnOnce(windows_sys::Win32::Foundation::HANDLE) -> io::Result<()>,
        Q: FnMut(&mut Child) -> io::Result<bool>,
        K: FnOnce(&mut Child) -> io::Result<()>,
    {
        use std::os::windows::io::AsRawHandle;

        let job = self.platform.job.as_raw_handle().cast();
        let mut first_error = None;
        let graceful_deadline = Instant::now() + grace;
        loop {
            match query_child(&mut self.child) {
                Ok(true) => break,
                Ok(false) => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                    break;
                }
            }
            if Instant::now() >= graceful_deadline {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let job_running = match query_job(job) {
            Ok(running) => running,
            Err(error) => {
                first_error.get_or_insert(error);
                true
            }
        };
        if job_running {
            if let Err(error) = terminate_job(job) {
                first_error.get_or_insert(error);
            }
        }

        let force_budget = grace.max(FALLBACK_CLEANUP);
        let job_deadline = Instant::now() + force_budget;
        let mut job_gone = false;
        loop {
            match query_job(job) {
                Ok(false) => {
                    job_gone = true;
                    break;
                }
                Ok(true) => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                    break;
                }
            }
            if Instant::now() >= job_deadline {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let child_gone = kill_and_reap_bounded_with(
            &mut self.child,
            Instant::now() + force_budget,
            &mut first_error,
            query_child,
            kill_child,
        );
        match query_job(job) {
            Ok(false) => job_gone = true,
            Ok(true) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
        if !job_gone {
            first_error.get_or_insert_with(|| {
                io::Error::new(io::ErrorKind::TimedOut, "Job Object survived bounded termination")
            });
        }
        if job_gone && child_gone {
            first_error.map_or(Ok(()), Err)
        } else {
            Err(first_error.expect("incomplete teardown records an error"))
        }
    }
    fn stop_platform(&mut self, grace: Duration) -> io::Result<()> {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        self.stop_with_windows_ops(
            grace,
            Self::query_job_running,
            |job| {
                // SAFETY: retained job is sole ownership evidence and remains live for this call.
                if unsafe { TerminateJobObject(job, 1) } == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            },
            |child| child.try_wait().map(|status| status.is_some()),
            Child::kill,
        )
    }
}


#[cfg(windows)]
fn resume_suspended_process(process_id: u32) -> io::Result<()> {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    // SAFETY: snapshot handle is checked and closed; entry size is initialized per ToolHelp contract.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut entry = THREADENTRY32 { dwSize: size_of::<THREADENTRY32>() as u32, ..Default::default() };
        let mut found = false;
        let mut current = Thread32First(snapshot, &mut entry);
        while current != 0 {
            if entry.th32OwnerProcessID == process_id {
                let thread_handle = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if thread_handle.is_null() {
                    let error = io::Error::last_os_error();
                    CloseHandle(snapshot);
                    return Err(error);
                }
                let resume_result = ResumeThread(thread_handle);
                let resume_error = (resume_result == u32::MAX).then(io::Error::last_os_error);
                CloseHandle(thread_handle);
                if let Some(error) = resume_error {
                    CloseHandle(snapshot);
                    return Err(error);
                }
                found = true;
            }
            current = Thread32Next(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
        found.then_some(()).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "suspended child thread not found"))
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        #[cfg(unix)]
        {
            let _ = self.signal_group(libc::SIGKILL);
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::JobObjects::TerminateJobObject;
            // SAFETY: retained job is sole ownership evidence and remains live for this call.
            let _ = unsafe { TerminateJobObject(self.platform.job.as_raw_handle().cast(), 1) };
        }
        let mut ignored = None;
        kill_and_reap_bounded(&mut self.child, Instant::now() + FALLBACK_CLEANUP, &mut ignored);
    }
}

#[cfg(test)]
mod tests {
    use super::ProcessTree;
    use std::fs;
    use std::io;
    use std::ops::{Deref, DerefMut};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    const HELPER_ROLE: &str = "MELON_PROCESS_TREE_HELPER_ROLE";
    const HELPER_READY: &str = "MELON_PROCESS_TREE_HELPER_READY";
    const HELPER_IGNORE_TERM: &str = "MELON_PROCESS_TREE_HELPER_IGNORE_TERM";
    const TEST_CHILD_CLEANUP: Duration = Duration::from_secs(1);

    fn cleanup_test_child(child: &mut Child) {
        let mut ignored = None;
        super::kill_and_reap_bounded(child, Instant::now() + TEST_CHILD_CLEANUP, &mut ignored);
    }


    struct ChildGuard(Child);

    impl Deref for ChildGuard {
        type Target = Child;
        fn deref(&self) -> &Self::Target { &self.0 }
    }

    impl DerefMut for ChildGuard {
        fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 }
    }

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            cleanup_test_child(&mut self.0);
        }
    }
    fn helper_command(role: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args(["--ignored", "--exact", "process_tree::tests::process_helper"])
            .env(HELPER_ROLE, role)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    fn unique_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!("melon-process-tree-{label}-{}-{nonce}", std::process::id()))
    }

    fn wait_for_ready(path: &Path) -> (u32, u32) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(text) = fs::read_to_string(path) {
                let mut ids = text.trim().split(',').map(|value| value.parse::<u32>().expect("pid"));
                return (ids.next().expect("parent pid"), ids.next().expect("grandchild pid"));
            }
            assert!(Instant::now() < deadline, "helper did not become ready: {}", path.display());
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "linux")]
    fn process_is_running(pid: u32) -> bool {
        let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return false,
            Err(_) => return true,
        };
        !matches!(stat.rsplit_once(") ").and_then(|(_, fields)| fields.chars().next()), Some('Z' | 'X'))
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    fn process_is_running(pid: u32) -> bool {
        let pid = i32::try_from(pid).expect("pid fits i32");
        // SAFETY: signal 0 probes this pid without delivering a signal.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[cfg(windows)]
    fn process_is_running(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        // SAFETY: returned handle is checked and closed before returning.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0;
            let running = GetExitCodeProcess(handle, &mut code) != 0 && code == STILL_ACTIVE as u32;
            CloseHandle(handle);
            running
        }
    }

    fn wait_gone(pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_is_running(pid) {
            assert!(Instant::now() < deadline, "pid {pid} survived teardown");
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn wait_direct(tree: &mut ProcessTree) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = tree.try_wait_direct().expect("direct child status") {
                return status;
            }
            assert!(Instant::now() < deadline, "direct child did not exit");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn observes_zero_and_nonzero_direct_status_without_timeout() {
        for code in [0, 37] {
            let mut command = helper_command("exit");
            command.env("MELON_PROCESS_TREE_HELPER_EXIT", code.to_string());
            let mut tree = ProcessTree::spawn(&mut command).expect("spawn exiting child");
            let started = Instant::now();
            let status = wait_direct(&mut tree);
            assert_eq!(status.code(), Some(code));
            assert!(started.elapsed() < Duration::from_secs(5));
            assert_eq!(tree.try_wait_direct().expect("repeated status"), Some(status));
            tree.stop(Duration::from_millis(50)).expect("stop observed child");
            tree.stop(Duration::from_millis(50)).expect("repeat stop observed child");
        }
    }

    #[test]
    fn direct_exit_does_not_hide_live_grandchild_tree() {
        let ready = unique_path("direct-exit-grandchild");
        let mut command = helper_command("parent-exit");
        command.env(HELPER_READY, &ready);
        let mut tree = ProcessTree::spawn(&mut command).expect("spawn parent-exit tree");
        let (_parent, grandchild) = wait_for_ready(&ready);
        assert_eq!(wait_direct(&mut tree).code(), Some(0));
        assert!(tree.is_running().expect("grandchild keeps tree running"));
        tree.stop(Duration::from_millis(100)).expect("stop remaining grandchild");
        wait_gone(grandchild);
        fs::remove_file(ready).expect("remove readiness file");
    }


    #[test]
    fn stops_parent_and_grandchild_without_touching_unrelated_process() {
        let ready = unique_path("tree");
        let mut command = helper_command("parent");
        command.env(HELPER_READY, &ready);
        let mut tree = ProcessTree::spawn(&mut command).expect("spawn owned tree");
        let (parent, grandchild) = wait_for_ready(&ready);
        let mut unrelated = ChildGuard(helper_command("leaf").spawn().expect("spawn unrelated helper"));

        assert!(tree.is_running().expect("tree liveness"));
        tree.stop(Duration::from_millis(500)).expect("stop tree");
        wait_gone(parent);
        wait_gone(grandchild);
        assert!(process_is_running(unrelated.id()), "unrelated helper was terminated");

        cleanup_test_child(&mut unrelated);
        fs::remove_file(ready).expect("remove readiness file");
    }

    #[test]
    fn stop_is_idempotent() {
        let ready = unique_path("idempotent");
        let mut command = helper_command("parent");
        command.env(HELPER_READY, &ready);
        let mut tree = ProcessTree::spawn(&mut command).expect("spawn owned tree");
        let _ = wait_for_ready(&ready);

        tree.stop(Duration::from_millis(500)).expect("first stop");
        tree.stop(Duration::from_millis(500)).expect("second stop");
        assert!(!tree.is_running().expect("stopped tree liveness"));
        fs::remove_file(ready).expect("remove readiness file");
    }

    #[cfg(unix)]
    #[test]
    fn escalates_when_tree_ignores_graceful_signal() {
        let ready = unique_path("forced");
        let mut command = helper_command("parent");
        command.env(HELPER_READY, &ready).env(HELPER_IGNORE_TERM, "1");
        let mut tree = ProcessTree::spawn(&mut command).expect("spawn owned tree");
        let (parent, grandchild) = wait_for_ready(&ready);

        tree.stop(Duration::from_millis(50)).expect("force stop tree");
        wait_gone(parent);
        wait_gone(grandchild);
        fs::remove_file(ready).expect("remove readiness file");
    }
    #[test]
    fn preserves_caller_environment_working_directory_and_stdio() {
        let directory = unique_path("cwd");
        fs::create_dir(&directory).expect("create working directory");
        #[cfg(unix)]
        let mut command = {
            let mut command = Command::new("sh");
            command.args(["-c", "printf '%s\\n%s\\n' \"$MELON_PROCESS_TREE_CALLER_VALUE\" \"$PWD\""]);
            command
        };
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("cmd.exe");
            command.args(["/D", "/S", "/C", "echo %MELON_PROCESS_TREE_CALLER_VALUE%&&cd"]);
            command
        };
        let output_path = directory.join("output.txt");
        let output_file = fs::File::create(&output_path).expect("create caller stdout");
        command
            .current_dir(&directory)
            .env("MELON_PROCESS_TREE_CALLER_VALUE", "caller-owned")
            .stdout(Stdio::from(output_file));
        let mut tree = ProcessTree::spawn(&mut command).expect("spawn inspect helper");
        let deadline = Instant::now() + Duration::from_secs(5);
        while fs::metadata(&output_path).expect("caller stdout metadata").len() == 0 {
            assert!(Instant::now() < deadline, "caller command did not write stdout");
            thread::sleep(Duration::from_millis(10));
        }
        tree.stop(Duration::from_secs(1)).expect("reap inspect helper");
        let output = fs::read_to_string(&output_path).expect("read caller stdout");
        assert_eq!(output, format!("caller-owned\n{}\n", directory.display()));
        fs::remove_file(output_path).expect("remove caller stdout");
        fs::remove_dir(directory).expect("remove working directory");
    }

    #[test]
    fn spawn_error_returns_no_owned_tree() {
        let mut command = Command::new(unique_path("missing-program"));
        assert!(ProcessTree::spawn(&mut command).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn isolation_failure_returns_no_owned_tree() {
        let mut command = helper_command("leaf");
        let error = ProcessTree::spawn_with_unix_isolation(&mut command, || {
            Err(io::Error::from_raw_os_error(libc::EPERM))
        })
        .expect_err("isolation failure must fail closed");
        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
    }

    #[cfg(windows)]
    #[test]
    fn assignment_failure_returns_no_owned_tree() {
        let mut command = helper_command("leaf");
        let error = ProcessTree::spawn_with_windows_assignment(&mut command, |_job, _process| {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "simulated assignment failure"))
        })
        .expect_err("job assignment failure must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
    #[cfg(windows)]
    #[test]
    fn termination_failure_returns_bounded_and_reaps_direct_child() {
        let mut command = helper_command("leaf");
        let mut tree = ProcessTree::spawn(&mut command).expect("spawn owned tree");
        let child = tree.child.id();
        let started = Instant::now();

        let error = tree
            .stop_with_windows_ops(
                Duration::from_millis(10),
                ProcessTree::query_job_running,
                |_job| Err(io::Error::new(io::ErrorKind::PermissionDenied, "simulated job termination failure")),
                |child| child.try_wait().map(|status| status.is_some()),
                std::process::Child::kill,
            )
            .expect_err("termination failure must be reported");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(started.elapsed() < Duration::from_secs(3), "stop exceeded its bounded fallback");
        wait_gone(child);
    }

    #[cfg(windows)]
    #[test]
    fn job_query_error_is_retained_while_termination_and_reap_continue() {
        use std::cell::Cell;
        let mut command = helper_command("leaf");
        let mut tree = ProcessTree::spawn(&mut command).expect("spawn owned tree");
        let queries = Cell::new(0);
        let terminated = Cell::new(false);

        let error = tree
            .stop_with_windows_ops(
                Duration::ZERO,
                |_job| {
                    let call = queries.get();
                    queries.set(call + 1);
                    if call == 0 {
                        Err(io::Error::new(io::ErrorKind::PermissionDenied, "first query failure"))
                    } else {
                        Ok(false)
                    }
                },
                |_job| {
                    terminated.set(true);
                    Ok(())
                },
                |child| child.try_wait().map(|status| status.is_some()),
                std::process::Child::kill,
            )
            .expect_err("first query error retained");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(terminated.get(), "query error skipped Job termination");
        assert!(queries.get() >= 2, "query error skipped final absence query");
        assert!(tree.child.try_wait().expect("child query").is_some(), "direct child was not reaped");
    }

    #[test]
    fn child_query_and_kill_errors_preserve_first_and_continue_polling() {
        use std::cell::Cell;
        let mut child = helper_command("leaf").spawn().expect("spawn helper");
        let queries = Cell::new(0);
        let killed = Cell::new(false);
        let mut first_error = None;

        let gone = super::kill_and_reap_bounded_with(
            &mut child,
            Instant::now() + Duration::from_millis(30),
            &mut first_error,
            |_child| {
                let call = queries.get();
                queries.set(call + 1);
                if call == 0 {
                    Err(io::Error::new(io::ErrorKind::PermissionDenied, "first child query failure"))
                } else {
                    Ok(false)
                }
            },
            |_child| {
                killed.set(true);
                Err(io::Error::new(io::ErrorKind::AlreadyExists, "later kill failure"))
            },
        );

        assert!(!gone);
        assert_eq!(first_error.expect("first error").kind(), io::ErrorKind::PermissionDenied);
        assert!(killed.get(), "query error skipped child kill");
        assert!(queries.get() > 1, "kill error skipped bounded reap polling");
        cleanup_test_child(&mut child);
    }
    #[test]
    #[ignore = "child process fixture"]
    fn process_helper() {
        match std::env::var(HELPER_ROLE).as_deref() {
            Ok("parent") => {
                #[cfg(unix)]
                if std::env::var_os(HELPER_IGNORE_TERM).is_some() {
                    // SAFETY: installing SIG_IGN is process-local and uses a valid signal number.
                    unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
                }
                let mut grandchild = helper_command("leaf");
                if std::env::var_os(HELPER_IGNORE_TERM).is_some() {
                    grandchild.env(HELPER_IGNORE_TERM, "1");
                }
                let grandchild = grandchild.spawn().expect("spawn grandchild");
                let ready = std::env::var_os(HELPER_READY).expect("readiness path");
                fs::write(ready, format!("{},{}", std::process::id(), grandchild.id())).expect("publish readiness");
                loop {
                    thread::park_timeout(Duration::from_secs(60));
                }
            }
            Ok("exit") => {
                let code = std::env::var("MELON_PROCESS_TREE_HELPER_EXIT")
                    .expect("exit code")
                    .parse::<i32>()
                    .expect("numeric exit code");
                std::process::exit(code);
            }
            Ok("parent-exit") => {
                let grandchild = helper_command("leaf").spawn().expect("spawn grandchild");
                let ready = std::env::var_os(HELPER_READY).expect("readiness path");
                fs::write(ready, format!("{},{}", std::process::id(), grandchild.id())).expect("publish readiness");
            }
            Ok("leaf") => {
                #[cfg(unix)]
                if std::env::var_os(HELPER_IGNORE_TERM).is_some() {
                    // SAFETY: installing SIG_IGN is process-local and uses a valid signal number.
                    unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
                }
                loop {
                    thread::park_timeout(Duration::from_secs(60));
                }
            }
            Ok("inspect") => {
                println!("{}", std::env::var("MELON_PROCESS_TREE_CALLER_VALUE").expect("caller env"));
                println!("{}", std::env::current_dir().expect("current directory").display());
            }
            role => panic!("unknown helper role: {role:?}"),
        }
    }
}
