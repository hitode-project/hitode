//! A player's single execution owner, independent of native views and sessions.
use adv_runtime::wine::session::Host;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};

type Job = Box<dyn FnOnce(&mut Host, &Path) + Send>;

/// Keeps Wine on one thread between games. Each submitted session must finish
/// before the player starts another; idle execution blocks on the job channel.
pub struct Execution {
    pub(crate) distribution: PathBuf,
    sender: Option<mpsc::SyncSender<Job>>,
    thread: Option<JoinHandle<()>>,
    running: Arc<AtomicBool>,
}

impl Execution {
    pub fn new(distribution: PathBuf) -> Result<Self, String> {
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
        raise_open_file_limit();
        let installation = distribution.clone();
        let (sender, receiver) = mpsc::sync_channel::<Job>(1);
        let thread = thread::Builder::new()
            .name("adv-execution".into())
            .spawn(move || {
                let mut host = Host::default();
                while let Ok(job) = receiver.recv() {
                    job(&mut host, &installation);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            distribution,
            sender: Some(sender),
            thread: Some(thread),
            running: Arc::new(AtomicBool::new(false)),
        })
    }

    pub(crate) fn submit(
        &self,
        job: impl FnOnce(&mut Host, &Path) + Send + 'static,
    ) -> Result<Task, String> {
        if self.running.swap(true, Ordering::AcqRel) {
            return Err("a game is already running".into());
        }
        let running = self.running.clone();
        let (done, result) = mpsc::sync_channel(1);
        let job = Box::new(move |host: &mut Host, distribution: &Path| {
            job(host, distribution);
            running.store(false, Ordering::Release);
            let _ = done.send(());
        });
        if self.sender.as_ref().unwrap().send(job).is_err() {
            self.running.store(false, Ordering::Release);
            return Err("execution owner stopped".into());
        }
        Ok(Task::Shared(result))
    }

    /// Imports a `.reg` file into the player's shared Wine prefix on the
    /// execution owner; scripts run as programs instead.
    pub fn import_registry(
        &self,
        options: crate::Options,
        file: PathBuf,
    ) -> Result<adv_runtime::wine::session::RegistryReport, String> {
        self.on_owner("registry import", move |host, distribution| {
            crate::wine::import_registry(host, &options, distribution, &file)
        })
    }
    /// The programs setups installed on the prefix of `options`, where they lie.
    pub fn installed(
        &self,
        options: crate::Options,
    ) -> Result<Vec<adv_runtime::wine::session::Installed>, String> {
        self.on_owner("listing installed programs", move |host, distribution| {
            crate::wine::installed(host, &options, distribution)
        })
    }
    /// Runs `work` on the execution owner while no game runs, and waits.
    fn on_owner<R: Send + 'static>(
        &self,
        what: &'static str,
        work: impl FnOnce(&mut Host, &Path) -> Result<R, String> + Send + 'static,
    ) -> Result<R, String> {
        let (tx, rx) = mpsc::sync_channel(1);
        let task = self.submit(move |host, distribution| {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(host, distribution)))
                    .unwrap_or_else(|_| Err(format!("{what} panicked")));
            let _ = tx.send(result);
        })?;
        task.join().map_err(|_| format!("{what} thread failed"))?;
        rx.recv()
            .map_err(|_| format!("{what} did not return a result"))?
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
fn raise_open_file_limit() {
    // The in-process Wine server's three pipes use six descriptors per guest
    // thread. Leave room for the game and the host UI as well.
    const OPEN_FILES: libc::rlim_t = 1024;
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: limit is writable for getrlimit and initialized for setrlimit.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 {
            let soft = limit.rlim_max.min(OPEN_FILES);
            if soft > limit.rlim_cur {
                limit.rlim_cur = soft;
                if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                    log::warn!("open file limit: {}", std::io::Error::last_os_error());
                }
            }
        }
    }
}

impl Drop for Execution {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) enum Task {
    Thread(JoinHandle<()>),
    Shared(mpsc::Receiver<()>),
}

impl Task {
    pub(crate) fn join(self) -> thread::Result<()> {
        match self {
            Self::Thread(thread) => thread.join(),
            Self::Shared(done) => done
                .recv()
                .map_err(|_| Box::new("execution owner stopped") as Box<dyn std::any::Any + Send>),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: HOST-SES-12
    #[test]
    fn games_share_one_owner_and_cannot_overlap() {
        let execution = Execution::new(PathBuf::from("authored-distribution")).unwrap();
        let (started, start) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let first = execution
            .submit(move |_, _| {
                started.send(thread::current().id()).unwrap();
                wait.recv().unwrap();
            })
            .unwrap();
        let owner = start.recv().unwrap();
        assert!(execution.submit(|_, _| {}).is_err());
        release.send(()).unwrap();
        first.join().unwrap();
        let (sent, received) = mpsc::channel();
        execution
            .submit(move |_, _| sent.send(thread::current().id()).unwrap())
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(received.recv().unwrap(), owner);
    }
}
