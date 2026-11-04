//! Test-only rendezvous immediately before the real phase sink takes LOG_LOCK.
use super::*;
use std::sync::{Condvar, Mutex as StdMutex, mpsc as channel};

pub(crate) static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static FAULT: StdMutex<Option<Arc<Stall>>> = StdMutex::new(None);
struct Stall {
    directory: PathBuf,
    entered: channel::Sender<()>,
    released: StdMutex<bool>,
    wake: Condvar,
}
pub(crate) struct HeldSink(Arc<Stall>);
impl HeldSink {
    pub(crate) fn release(&self) {
        *self.0.released.lock().unwrap() = true;
        self.0.wake.notify_all();
    }
    // External observation always releases real contention, even on RED.
    pub(crate) fn observe(
        self,
        entered: channel::Receiver<()>,
        done: channel::Receiver<()>,
    ) -> bool {
        if entered.recv_timeout(Duration::from_secs(5)).is_err() {
            return false;
        }
        let lock = LOG_LOCK.lock().unwrap();
        self.release();
        let completed = done.recv_timeout(Duration::from_millis(500)).is_ok();
        drop(lock);
        completed
    }
}
impl Drop for HeldSink {
    fn drop(&mut self) {
        self.release();
        *FAULT.lock().unwrap() = None;
    }
}
pub(crate) fn hold(directory: &Path) -> (HeldSink, channel::Receiver<()>) {
    let (entered, receiver) = channel::channel();
    let stall = Arc::new(Stall {
        directory: directory.into(),
        entered,
        released: StdMutex::new(false),
        wake: Condvar::new(),
    });
    let mut fault = FAULT.lock().unwrap();
    assert!(fault.is_none());
    *fault = Some(stall.clone());
    (HeldSink(stall), receiver)
}
pub(crate) fn before_append(directory: &Path) {
    let stall = FAULT
        .lock()
        .unwrap()
        .as_ref()
        .filter(|s| s.directory == directory)
        .cloned();
    if let Some(stall) = stall {
        let _ = stall.entered.send(());
        let mut released = stall.released.lock().unwrap();
        while !*released {
            released = stall.wake.wait(released).unwrap();
        }
    }
}
