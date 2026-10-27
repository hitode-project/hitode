# Thread ownership and background tasks

`background::Builder` registers up to eight independent SPSC producer ports at startup.
Each port's urgent/background queues hold 1–64 items; `start()` creates one `adv-background` thread.
`Sender` can move to an execution or UI owner but is neither Clone nor Sync.
Components sharing an execution owner can use `into_local()` for a local `Rc<RefCell<Sender>>`.
There is no cross-thread MPSC lock.

`try_submit(Priority, Task)` never waits. A full or closed queue returns the original Task and its resources.
After rejection, `sender_with_capacity_wake` wakes the owner when the first slot becomes available, allowing retries without polling.
The blocked marker is published before checking capacity; the service shutdown gate also counts producers publishing tasks.
The worker exits only after observing shutdown, zero producers, and then empty queues.
An earlier empty scan cannot justify exit: accepted tasks might be published afterward. Deterministic tests enforce this shutdown order.

`Task::new` accepts a one-shot `FnOnce + Send`. Urgent tasks take priority; equal-priority owners rotate.
Completion signals the single execution WakeEvent by default. UI tasks use `with_wake(ui_wake)`.
Tasks with typed completion callbacks use `without_wake()` to avoid duplicate notifications.
Results and terminal Ticket states precede wakeups; task panics are contained, and the worker continues.
`affine_to(sender.affinity())` binds tasks carrying worker-local state, preventing submission to another service.

Tickets cancel only queued, discardable tasks; they do not preempt active jobs.
GPU map/presenter tasks requiring completion and resource return use `finish_on_shutdown()`.
Once accepted, these tasks run during shutdown and cannot be cancelled through Tickets.
`Service::shutdown` is for teardown: it cancels ordinary queued tasks, reclaims required jobs, and joins the worker.
It can wait for active compile/acquire/fence operations and must not serve as enqueue backpressure.
Keep the window alive until these tasks and their results are reclaimed.
Callers handle unaccepted required Tasks; the service does not replace device error protocols.

`Builder::with_work_policy(WorkPolicy::responsive(units))` explicitly configures startup; the default has no work limit.
Submitters interpret these neutral units. They are neither milliseconds nor forced preemption for arbitrary FnOnce tasks.
Registered ports read the same immutable configuration.
This policy does not bound GPU acquire/fence blocking time.

```sh
cargo test -p adv-sync --release
```
