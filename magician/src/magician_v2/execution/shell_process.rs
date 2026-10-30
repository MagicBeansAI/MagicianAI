//! Cleanup for process groups created by the shell executor itself.

pub(crate) struct ProcessGroup {
    pid: Option<u32>,
}

impl ProcessGroup {
    pub(crate) fn configure(command: &mut tokio::process::Command) {
        command.kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
    }

    pub(crate) fn new(child: &tokio::process::Child) -> Self {
        Self { pid: child.id() }
    }

    pub(crate) fn terminate(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid.take() {
            // The group was created for this child, never supplied by the model.
            if let Ok(pid) = i32::try_from(pid) {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}
