//! One bounded pixel job at a time. Never owns or writes the session socket.
use super::{POLL, SessionError};
use crate::{
    framebuffer::{AppliedFrame, Framebuffer},
    protocol::FrameUpdate,
};
use std::{
    io,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

struct Job {
    image: Arc<RwLock<Framebuffer>>,
    payload: Vec<u8>,
}

pub(super) struct Decoder {
    jobs: Option<SyncSender<Job>>,
    results: Receiver<Result<AppliedFrame, SessionError>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    busy: bool,
}
impl Decoder {
    pub fn start() -> io::Result<Self> {
        let (jobs, receive) = mpsc::sync_channel::<Job>(1);
        let (send, results) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let worker = thread::Builder::new()
            .name("remote-screen-decoder".into())
            .spawn(move || {
                while let Ok(job) = receive.recv() {
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    if send.send(decode(job, &cancelled)).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            jobs: Some(jobs),
            results,
            stop,
            worker: Some(worker),
            busy: false,
        })
    }
    /// The socket owner waits for this result before submitting another frame or
    /// handling reconfiguration. No delta is dropped, copied, reordered or ACKed early.
    pub fn submit(
        &mut self,
        image: Arc<RwLock<Framebuffer>>,
        payload: Vec<u8>,
    ) -> Result<(), SessionError> {
        if self.busy {
            return Err(SessionError::Local("Frame decode already in flight".into()));
        }
        self.jobs
            .as_ref()
            .unwrap()
            .try_send(Job { image, payload })
            .map_err(|_| SessionError::Local("Frame decoder unavailable".into()))?;
        self.busy = true;
        Ok(())
    }
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub fn receive(&mut self, timeout: Duration) -> Result<Option<AppliedFrame>, SessionError> {
        match self.results.recv_timeout(timeout) {
            Ok(result) => {
                self.busy = false;
                result.map(Some)
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                Err(SessionError::Local("Frame decoder stopped".into()))
            }
        }
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        self.request_stop();
        drop(self.jobs.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn decode(job: Job, stop: &AtomicBool) -> Result<AppliedFrame, SessionError> {
    let frame = FrameUpdate::parse(&job.payload)?;
    loop {
        if stop.load(Ordering::Acquire) {
            return Err(SessionError::Cancelled);
        }
        match job.image.try_write() {
            Ok(mut image) => {
                return image
                    .apply_unless(&frame, || stop.load(Ordering::Acquire))
                    .ok_or(SessionError::Cancelled);
            }
            Err(std::sync::TryLockError::WouldBlock) => thread::sleep(POLL),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(SessionError::Local("Framebuffer lock failed".into()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{PixelFormat, ServerInit};
    use std::time::Instant;

    fn image() -> Arc<RwLock<Framebuffer>> {
        Arc::new(RwLock::new(
            Framebuffer::new(ServerInit {
                width: 2,
                height: 2,
                rotation: 0,
                pixel_format: PixelFormat([32, 16, 8, 8, 8, 0, 8]),
                name: "synthetic".into(),
                input_capabilities: 7,
                audio_available: false,
            })
            .unwrap(),
        ))
    }
    fn empty_frame() -> Vec<u8> {
        let mut payload = 42u32.to_le_bytes().to_vec();
        payload.extend([0, 0, 0]);
        payload
    }
    #[test]
    fn one_in_flight_frame_and_no_early_completion_while_a_reader_holds_pixels() {
        let mut decoder = Decoder::start().unwrap();
        let image = image();
        let guard = image.read().unwrap();
        decoder.submit(image.clone(), empty_frame()).unwrap();
        assert!(decoder.submit(image.clone(), empty_frame()).is_err());
        assert!(
            decoder
                .receive(Duration::from_millis(100))
                .unwrap()
                .is_none()
        );
        drop(guard);
        let result = decoder.receive(Duration::from_secs(2)).unwrap().unwrap();
        assert_eq!(result.id, 42);
        assert_eq!(result.applied, 0);
        decoder.submit(image, empty_frame()).unwrap();
        assert!(decoder.receive(Duration::from_secs(2)).unwrap().is_some());
    }
    #[test]
    fn malformed_frames_keep_protocol_errors() {
        let mut decoder = Decoder::start().unwrap();
        decoder.submit(image(), vec![0]).unwrap();
        assert!(matches!(
            decoder.receive(Duration::from_secs(2)),
            Err(SessionError::Protocol(_))
        ));
    }
    #[test]
    fn stopping_decoder_does_not_wait_for_a_framebuffer_reader() {
        let mut decoder = Decoder::start().unwrap();
        let image = image();
        let _guard = image.read().unwrap();
        decoder.submit(image.clone(), empty_frame()).unwrap();
        assert!(
            decoder
                .receive(Duration::from_millis(100))
                .unwrap()
                .is_none()
        );
        let start = Instant::now();
        drop(decoder);
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
