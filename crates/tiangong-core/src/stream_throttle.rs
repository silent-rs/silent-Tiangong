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

/// 流式文本所属的阶段，决定 flush 时发送的 StreamEvent 变体。
#[derive(Debug, Clone, Copy)]
pub(crate) enum StreamTextKind {
    /// 普通文本增量（向后兼容，发送 Delta）。保留供未标记阶段的调用方使用。
    #[allow(dead_code)]
    Delta,
    /// ReAct 工具执行阶段的过程性文本（发送 ReactText）
    React,
    /// 历史总结阶段的最终回复（发送 SummaryText）。任务 15 起不再发起独立
    /// Summary 请求，该变体保留以维持前端事件契约；最终回复现在以 ReactText
    /// 流出并经消息 upsert（phase=Summary）提交，任务 18 前端联调后定去留。
    #[allow(dead_code)]
    Summary,
}

#[derive(Default)]
struct StreamBuffers {
    content: String,
    reasoning: String,
}

/// 将 LLM token delta 合并为固定节奏的 StreamEvent。
pub(crate) struct ThrottledStreamSink {
    message_id: String,
    tx: Sender<StreamEvent>,
    text_kind: StreamTextKind,
    buffers: Arc<Mutex<StreamBuffers>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ThrottledStreamSink {
    /// 创建默认（Delta）文本类型的 sink。保留作为向后兼容入口。
    #[allow(dead_code)]
    pub(crate) fn new(message_id: String, tx: Sender<StreamEvent>) -> Self {
        Self::with_text_kind(message_id, tx, StreamTextKind::Delta)
    }

    /// 指定文本阶段类型，决定 flush 时发送 ReactText / SummaryText / Delta。
    pub(crate) fn with_text_kind(
        message_id: String,
        tx: Sender<StreamEvent>,
        text_kind: StreamTextKind,
    ) -> Self {
        Self::with_interval(message_id, tx, text_kind, DEFAULT_FLUSH_INTERVAL)
    }

    fn with_interval(
        message_id: String,
        tx: Sender<StreamEvent>,
        text_kind: StreamTextKind,
        interval: Duration,
    ) -> Self {
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
                    flush_buffers(&worker_buffers, &worker_tx, &worker_message_id, text_kind);
                }
                flush_buffers(&worker_buffers, &worker_tx, &worker_message_id, text_kind);
            })
            .ok();

        Self {
            message_id,
            tx,
            text_kind,
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
            flush_buffers(&self.buffers, &self.tx, &self.message_id, self.text_kind);
        }
    }
}

impl Drop for ThrottledStreamSink {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

fn flush_buffers(
    buffers: &Arc<Mutex<StreamBuffers>>,
    tx: &Sender<StreamEvent>,
    message_id: &str,
    text_kind: StreamTextKind,
) {
    // 持锁直到事件发送完成，使显式 flush 返回时与定时 flush 建立严格先后关系。
    let Ok(mut buffers) = buffers.lock() else {
        return;
    };
    let content = mem::take(&mut buffers.content);
    let reasoning = mem::take(&mut buffers.reasoning);

    if !content.is_empty() {
        let event = match text_kind {
            StreamTextKind::Delta => StreamEvent::Delta {
                message_id: message_id.to_string(),
                content,
            },
            StreamTextKind::React => StreamEvent::ReactText {
                message_id: message_id.to_string(),
                content,
            },
            StreamTextKind::Summary => StreamEvent::SummaryText {
                message_id: message_id.to_string(),
                content,
            },
        };
        let _ = tx.send(event);
    }
    if !reasoning.is_empty() {
        let _ = tx.send(StreamEvent::Reasoning {
            message_id: message_id.to_string(),
            content: reasoning,
        });
    }
}
