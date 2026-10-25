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
