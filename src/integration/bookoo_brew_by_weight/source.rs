//! Receive continuously, preserving callback times even during a GATT write.
use super::wire::{FRESH, Sample};
use crate::integration::bluetooth_scale::bluez::{Session, Writer};
use anyhow::Context;
use std::time::Instant;
use tokio::sync::mpsc;
pub(super) struct Source {
    rx: mpsc::Receiver<(anyhow::Result<Vec<u8>>, Instant)>,
    task: tokio::task::JoinHandle<()>,
    pub writer: Writer,
}
impl Source {
    pub fn new(mut session: Session) -> Self {
        let writer = session.writer();
        let (tx, rx) = mpsc::channel(32);
        let task = tokio::spawn(async move {
            loop {
                let result = tokio::time::timeout(FRESH, session.packet())
                    .await
                    .context("BOOKOO readings timed out")
                    .and_then(|r| r);
                let failed = result.is_err();
                if tx.try_send((result, Instant::now())).is_err() || failed {
                    break;
                }
            }
            session.close().await;
        });
        Self { rx, task, writer }
    }
    pub async fn next(&mut self) -> anyhow::Result<(Sample, Instant)> {
        anyhow::ensure!(
            !self.task.is_finished(),
            "BOOKOO stream ended or overloaded; stop the Mini manually"
        );
        let (bytes, at) = self.rx.recv().await.context("BOOKOO stream ended")?;
        anyhow::ensure!(
            at.elapsed() <= FRESH,
            "BOOKOO reading arrived too late; stop the Mini manually"
        );
        Ok((Sample::decode(&bytes?)?, at))
    }
    pub async fn latest(&mut self) -> anyhow::Result<(Sample, Instant)> {
        let mut latest = self.next().await?;
        while let Ok((bytes, at)) = self.rx.try_recv() {
            latest = (Sample::decode(&bytes?)?, at);
        }
        anyhow::ensure!(latest.1.elapsed() <= FRESH, "BOOKOO source is stale");
        Ok(latest)
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        self.task.abort();
    }
}
