//! Turn 运行时间旁路通知。

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use tiangong_types::StreamEvent;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

pub(super) struct TurnElapsedTimer {
    stop_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl TurnElapsedTimer {
    pub(super) fn start(started_at: Instant, stream_tx: Sender<StreamEvent>) -> Self {
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            ticker.tick().await;

            loop {
                tokio::select! {
                    biased;
                    _ = &mut stop_rx => break,
                    _ = ticker.tick() => {
                        let seconds = started_at.elapsed().as_secs();
                        if stream_tx.send(StreamEvent::TurnElapsed { seconds }).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Self {
            stop_tx: Some(stop_tx),
            task: Some(task),
        }
    }

    pub(super) async fn stop(mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for TurnElapsedTimer {
    fn drop(&mut self) {
        self.stop_tx.take();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn emits_elapsed_seconds_through_stream() {
        let (stream_tx, stream_rx) = std::sync::mpsc::channel();
        let timer = TurnElapsedTimer::start(Instant::now(), stream_tx);
        let event =
            tokio::task::spawn_blocking(move || stream_rx.recv_timeout(Duration::from_secs(3)))
                .await
                .expect("计时事件接收任务不应失败")
                .expect("计时器应在一秒后发送事件");
        timer.stop().await;

        assert!(matches!(
            event,
            StreamEvent::TurnElapsed { seconds } if seconds >= 1
        ));
    }
}
