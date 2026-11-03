//! Package C2: the GPU owner thread. It drains command chunks, groups the
//! commands between `Target`s into passes with [`super::pass::plan`] once the
//! next target or a submission boundary is known, encodes them into one
//! encoder per submission, retains each chunk's ring spans with that encoder,
//! submits at `Present`, `Readback` and `Submit`, and hands frames to the
//! [`crate::frame::Presenter`] on this thread.

use super::stream::Consumer;
use crate::frame::Presenter;

pub(super) fn run(queue: adv_gpu::Queue, commands: Consumer, presenter: Box<dyn Presenter>) {
    let _ = (queue, commands, presenter);
    todo!("C2")
}
