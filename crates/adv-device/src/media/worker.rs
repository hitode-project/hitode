//! A decoder thread per source. Each request still gets the answer the decoder
//! would give on the caller's thread; between requests the thread decodes and
//! converts the frames the next request is likely to need, so the caller (the
//! guest's single execution thread) waits only for work it could not foresee.

use std::sync::mpsc::{Receiver, Sender, channel};

use super::native::Native;
use super::{Error, ReadSeek, Stats, StreamInfo, Update};

enum Request {
    At(u64),
    Seek(u64),
}

type Reply = (Result<Option<Update>, Error>, Stats);

pub(super) struct Worker {
    requests: Sender<Request>,
    replies: Receiver<Reply>,
    pending: bool,
    seeks: usize,
    pub info: StreamInfo,
    pub stats: Stats,
}

// SAFETY: FFmpeg contexts have no thread affinity; `Native` moves to its
// thread once and is used only there.
struct Owned(Native);
unsafe impl Send for Owned {}

impl Worker {
    pub fn open(reader: Box<dyn ReadSeek>) -> Result<Self, Error> {
        let native = Native::open(reader)?;
        let (info, stats) = (native.info.clone(), native.stats);
        let (requests, inbox) = channel();
        let (outbox, replies) = channel();
        let owned = Owned(native);
        std::thread::Builder::new()
            .name("media-decode".into())
            .spawn(move || serve(owned, &inbox, &outbox))?;
        Ok(Self {
            requests,
            replies,
            pending: false,
            seeks: 0,
            info,
            stats,
        })
    }

    fn call(&mut self, request: Request) -> Result<Option<Update>, Error> {
        let lost = || Error::Decode("decoder thread ended".into());
        self.requests.send(request).map_err(|_| lost())?;
        let (result, stats) = self.replies.recv().map_err(|_| lost())?;
        self.stats = stats;
        result
    }

    pub fn poll_frame(&mut self, media_us: u64) -> Result<Option<Update>, Error> {
        let lost = || Error::Decode("decoder thread ended".into());
        while self.seeks != 0 {
            match self.replies.try_recv() {
                Ok((result, stats)) => {
                    self.stats = stats;
                    if result?.is_some() {
                        self.pending = false;
                    } else {
                        self.seeks -= 1;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(None),
                Err(_) => return Err(lost()),
            }
        }
        if !self.pending {
            self.requests
                .send(Request::At(media_us))
                .map_err(|_| lost())?;
            self.pending = true;
        }
        match self.replies.try_recv() {
            Ok((result, stats)) => {
                self.pending = false;
                self.stats = stats;
                result
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(None),
            Err(_) => Err(lost()),
        }
    }

    pub fn request_seek(&mut self, media_us: u64) -> Result<(), Error> {
        // Discard an outstanding frame before the ordered seek response.
        self.requests
            .send(Request::Seek(media_us))
            .map_err(|_| Error::Decode("decoder thread ended".into()))?;
        self.seeks += 1;
        Ok(())
    }

    pub fn frame_at(&mut self, media_us: u64) -> Result<Update, Error> {
        Ok(self.call(Request::At(media_us))?.unwrap_or_default())
    }

    pub fn seek(&mut self, media_us: u64) -> Result<(), Error> {
        self.call(Request::Seek(media_us)).map(drop)
    }
}

fn serve(owned: Owned, inbox: &Receiver<Request>, outbox: &Sender<Reply>) {
    let Owned(mut native) = owned;
    // Ends when the `Worker` is dropped.
    while let Ok(request) = inbox.recv() {
        let result = match request {
            Request::At(media_us) => native.frame_at(media_us).map(Some),
            Request::Seek(media_us) => native.seek(media_us).map(|()| None),
        };
        let failed = result.is_err();
        if outbox.send((result, native.stats)).is_err() {
            return;
        }
        if !failed {
            native.prefetch();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a caller can observe of one answer.
    fn answer(update: Result<Update, Error>) -> String {
        match update {
            Ok(u) => format!(
                "{:?} {:?} {}",
                u.video.map(|v| (
                    v.pts_us,
                    v.bgra.len(),
                    v.bgra.iter().map(|&b| u64::from(b)).sum::<u64>()
                )),
                u.audio
                    .iter()
                    .map(|a| (a.pts_us, a.samples.len()))
                    .collect::<Vec<_>>(),
                u.ended
            ),
            Err(error) => error.to_string(),
        }
    }

    // spec: MEDIA-STREAM-4
    #[test]
    fn prefetching_between_requests_changes_no_answer() {
        let movie =
            include_bytes!("../../../adv-dx/src/dshow/graph/render/fixtures/guest_movie.mpg");
        // Forward steps, a repeat, a skipped frame, a step back, a jump and a
        // rewind, then past the end.
        let requests = [
            0, 10_000, 40_000, 40_000, 125_000, 90_000, 1_300_000, 0, 80_000, 400_000,
        ];
        let run = |prefetch: bool| -> Vec<String> {
            let reader = Box::new(std::io::Cursor::new(movie.to_vec()));
            let mut native = Native::open(reader).unwrap();
            requests
                .iter()
                .map(|&t| {
                    let answer = answer(native.frame_at(t));
                    if prefetch {
                        native.prefetch();
                    }
                    answer
                })
                .collect()
        };
        assert_eq!(run(true), run(false));
        let mut worker = Worker::open(Box::new(std::io::Cursor::new(movie.to_vec()))).unwrap();
        let threaded: Vec<String> = requests
            .iter()
            .map(|&t| answer(worker.frame_at(t)))
            .collect();
        assert_eq!(threaded, run(false));
    }
}
