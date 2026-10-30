//! An owned child and its isolated Unix process group. npm CLI wrappers can
//! spawn a callback server, so terminating only the wrapper would orphan it.
use std::ops::{Deref, DerefMut};
use tokio::process::{Child, Command};

pub(super) struct OwnedProcess(Child);
impl OwnedProcess {
    pub fn spawn(command: &mut Command) -> std::io::Result<Self> {
        #[cfg(unix)]
        command.process_group(0);
        command.kill_on_drop(true).spawn().map(Self)
    }

    fn kill_group(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0.id().and_then(|pid| i32::try_from(pid).ok()) {
            // SAFETY: spawn created a new process group with this owned child
            // as leader. It has not been reaped, so its id cannot be reused.
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
        }
        let _ = self.0.start_kill();
    }

    pub async fn terminate(&mut self) {
        self.kill_group();
        let _ = self.0.wait().await;
    }
}
impl Deref for OwnedProcess {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl DerefMut for OwnedProcess {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        self.kill_group();
    }
}
