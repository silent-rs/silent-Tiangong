//! LLM 流式输出节流。
//!
//! 高速本地或局域网模型可能在极短时间内产生大量 token delta。
//! 这里将 token 先累积到内存缓冲，再按固定节奏向外发送，避免前端事件队列被打爆。

use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tiangong_types::StreamEvent;

use crate::model::ModelStreamChunk;

const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Default)]
struct StreamBuffers {
    content: String,
    reasoning: String,
}

/// 将 LLM token delta 合并为固定节奏的 StreamEvent。
pub(crate) struct ThrottledStreamSink {
    message_id: String,
    tx: Sender<StreamEvent>,
    buffers: Arc<Mutex<StreamBuffers>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ThrottledStreamSink {
    pub(crate) fn new(message_id: String, tx: Sender<StreamEvent>) -> Self {
        Self::with_interval(message_id, tx, DEFAULT_FLUSH_INTERVAL)
    }

    fn with_interval(message_id: String, tx: Sender<StreamEvent>, interval: Duration) -> Self {
        let buffers = Arc::new(Mutex::new(StreamBuffers::default()));
        let stop = Arc::new(AtomicBool::new(false));

        let worker_buffers = buffers.clone();
        let worker_stop = stop.clone();
        let worker_tx = tx.clone();
        let worker_message_id = message_id.clone();
        let worker = thread::Builder::new()
            .name("tiangong-llm-stream-throttle".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    thread::sleep(interval);
                    flush_buffers(&worker_buffers, &worker_tx, &worker_message_id);
                }
                flush_buffers(&worker_buffers, &worker_tx, &worker_message_id);
            })
            .ok();

        Self {
            message_id,
            tx,
            buffers,
            stop,
            worker,
        }
    }

    pub(crate) fn push_chunk(&self, chunk: &ModelStreamChunk) {
        if chunk.content.is_empty() && chunk.reasoning_content.is_empty() {
            return;
        }

        let Ok(mut buffers) = self.buffers.lock() else {
            return;
        };
        buffers.content.push_str(&chunk.content);
        buffers.reasoning.push_str(&chunk.reasoning_content);
    }

    pub(crate) fn finish(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        } else {
            flush_buffers(&self.buffers, &self.tx, &self.message_id);
        }
    }
}

impl Drop for ThrottledStreamSink {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

fn flush_buffers(buffers: &Arc<Mutex<StreamBuffers>>, tx: &Sender<StreamEvent>, message_id: &str) {
    let (content, reasoning) = {
        let Ok(mut buffers) = buffers.lock() else {
            return;
        };
        (
            mem::take(&mut buffers.content),
            mem::take(&mut buffers.reasoning),
        )
    };

    if !content.is_empty() {
        let _ = tx.send(StreamEvent::Delta {
            message_id: message_id.to_string(),
            content,
        });
    }
    if !reasoning.is_empty() {
        let _ = tx.send(StreamEvent::Reasoning {
            message_id: message_id.to_string(),
            content: reasoning,
        });
    }
}
