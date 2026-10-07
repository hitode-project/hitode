//! Process-family service transport. Requests own their arguments and cross a
//! machine boundary before touching a second address space. The runtime supplies
//! machines; ABI validation remains in the personality which submits each task.
use adv_machine::{
    Env, Outcome,
    process::{ProcessIdentity, ThreadIdentity},
};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Weak};

pub const MAX_LIVE_PROCESSES: usize = 8;
pub const MAX_PROCESSES: usize = 128;
const MAX_REQUESTS: usize = 64;

#[derive(Clone, Debug)]
pub struct Error {
    pub code: u32,
    pub message: String,
}
impl Error {
    pub fn new(code: u32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
pub type Reply<T> = Rc<RefCell<Option<Result<T, Error>>>>;

#[derive(Clone)]
pub struct Spawn {
    pub parent: u32,
    pub config: crate::OsConfig,
    pub command_line: String,
    pub directory: String,
    pub environment: Vec<String>,
    pub suspended: bool,
    pub inherited: Vec<InheritedHandle>,
}
#[derive(Clone)]
pub enum InheritedObject {
    Process(Arc<ProcessIdentity>, u32),
    Thread(ThreadIdentity, u32),
}
#[derive(Clone)]
pub struct InheritedHandle {
    pub handle: u32,
    pub flags: u32,
    pub object: InheritedObject,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Publication {
    Pending,
    Published,
    Cancelled,
}
pub struct Created {
    pub process: Arc<ProcessIdentity>,
    pub thread: ThreadIdentity,
    /// The child cannot execute until the caller publishes both handles and PI.
    pub publication: Rc<Cell<Publication>>,
}
impl Drop for Created {
    fn drop(&mut self) {
        if self.publication.get() == Publication::Pending {
            self.publication.set(Publication::Cancelled);
        }
    }
}
#[derive(Clone)]
pub struct Parameters {
    pub command_line: String,
    pub environment: Vec<String>,
}

pub type Visit = Box<dyn FnOnce(Result<&mut Env<'_>, Error>)>;
pub enum Request {
    Spawn {
        spec: Box<Spawn>,
        reply: Reply<Created>,
    },
    Visit {
        pid: u32,
        context: Option<u32>,
        task: Visit,
    },
}
#[derive(Default)]
struct Shared {
    requests: VecDeque<Request>,
    directory: BTreeMap<u32, Weak<ProcessIdentity>>,
    parents: BTreeMap<u32, u32>,
}
#[derive(Clone, Default)]
pub struct Services(Rc<RefCell<Shared>>);
impl Services {
    pub fn register(&self, process: &Arc<ProcessIdentity>) {
        self.0
            .borrow_mut()
            .directory
            .insert(process.pid, Arc::downgrade(process));
    }
    pub fn register_child(&self, process: &Arc<ProcessIdentity>, parent: u32) {
        self.register(process);
        self.0.borrow_mut().parents.insert(process.pid, parent);
    }
    /// Live guest identities and their guest parents, in stable PID order.
    pub fn live_processes(&self) -> Vec<(u32, u32)> {
        let shared = self.0.borrow();
        shared
            .directory
            .iter()
            .filter_map(|(&pid, identity)| {
                let identity = identity.upgrade()?;
                identity
                    .completion
                    .exit_code()
                    .is_none()
                    .then(|| (pid, shared.parents.get(&pid).copied().unwrap_or(0)))
            })
            .collect()
    }
    pub fn find(&self, pid: u32) -> Option<Arc<ProcessIdentity>> {
        self.0.borrow().directory.get(&pid)?.upgrade()
    }
    pub fn pop(&self) -> Option<Request> {
        self.0.borrow_mut().requests.pop_front()
    }
    pub fn pending(&self) -> bool {
        !self.0.borrow().requests.is_empty()
    }
    pub fn push(&self, request: Request) -> Result<(), Error> {
        let mut shared = self.0.borrow_mut();
        if shared.requests.len() >= MAX_REQUESTS {
            return Err(Error::new(8, "process request limit"));
        }
        shared.requests.push_back(request);
        Ok(())
    }
}

/// Execute a personality-owned operation on a stopped target machine, returning
/// its result to the original caller without borrowing either machine twice.
pub fn visit<T: 'static>(
    env: &mut Env<'_>,
    pid: u32,
    context: Option<u32>,
    task: impl FnOnce(&mut Env<'_>) -> Result<T, Error> + 'static,
    complete: impl FnOnce(&mut Env<'_>, Result<T, Error>) -> Outcome + 'static,
) -> Outcome {
    if pid == env.sys.pid && context.is_none() {
        let result = task(env);
        return complete(env, result);
    }
    let Some(service) = env.ext.try_get::<Services>().cloned() else {
        return complete(
            env,
            Err(Error::new(50, "process supervisor is unavailable")),
        );
    };
    let reply: Reply<T> = Rc::new(RefCell::new(None));
    let output = reply.clone();
    let request = Request::Visit {
        pid,
        context,
        task: Box::new(move |target| {
            *output.borrow_mut() = Some(target.and_then(task));
        }),
    };
    if let Err(error) = service.push(request) {
        return complete(env, Err(error));
    }
    Outcome::host_request(move |env| {
        let result = reply
            .borrow_mut()
            .take()
            .unwrap_or_else(|| Err(Error::new(50, "process request was not serviced")));
        complete(env, result)
    })
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn live_catalog_preserves_guest_parents_and_excludes_exited_or_dropped_owners() {
        let services = Services::default();
        let parent = ProcessIdentity::new(11);
        let child = ProcessIdentity::new(22);
        services.register(&parent);
        services.register_child(&child, parent.pid);
        {
            let temporary = ProcessIdentity::new(33);
            services.register(&temporary);
        }
        assert_eq!(services.live_processes(), [(11, 0), (22, 11)]);
        child.completion.complete(37);
        assert_eq!(services.live_processes(), [(11, 0)]);
        drop(parent);
        assert!(services.live_processes().is_empty());
    }
}
