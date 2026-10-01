//! Owns the monitor handles on one dedicated thread (DDC handles aren't Send) and
//! runs jobs against them. A failed job re-enumerates monitors and retries once,
//! which covers hotplug and monitors that were asleep at startup.

use std::sync::mpsc;

use anyhow::{anyhow, Result};

use crate::monitors::Monitors;

type Job = Box<dyn FnOnce(&mut Monitors) + Send>;

pub struct DdcWorker {
    tx: mpsc::Sender<Job>,
}

impl DdcWorker {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("ddc".into())
            .spawn(move || {
                let mut monitors = Monitors::enumerate();
                for job in rx {
                    job(&mut monitors);
                }
            })
            .expect("spawn ddc thread");
        DdcWorker { tx }
    }

    pub fn run<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: Fn(&mut Monitors) -> Result<T> + Send + 'static,
    {
        let (rtx, rrx) = mpsc::channel();
        self.tx
            .send(Box::new(move |m: &mut Monitors| {
                let result = f(m).or_else(|_| {
                    *m = Monitors::enumerate();
                    f(m)
                });
                let _ = rtx.send(result);
            }))
            .map_err(|_| anyhow!("DDC worker stopped"))?;
        rrx.recv().map_err(|_| anyhow!("DDC worker stopped"))?
    }

    pub fn refresh(&self) -> Result<()> {
        self.run(|m| {
            *m = Monitors::enumerate();
            Ok(())
        })
    }
}
