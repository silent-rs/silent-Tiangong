//! 模型请求策略：请求前上下文准备与请求错误恢复（ALR-303/304，任务 16）。
//!
//! 压缩不再作为 Agent 顶层阶段存在：请求前压力检查与上下文溢出恢复都在本
//! 模块内联完成，Loop 只关心「发起请求 → 处理响应」。压缩期间通过统一的
//! [`ContextCompression::run`] 监听命令通道：命令到达时中止压缩并交还执行
//! 驱动接管，压缩不会改变 Agent 对外的 `Idle | Running` 状态。
//!
//! Provider 临时错误的退避重试由模型客户端（`SingleProviderClient::with_on_retry`）
//! 负责，Loop 不重复这些判断；本模块只处理需要本地动作的两类错误：
//! 上下文溢出（压缩后重试）与不可恢复错误（明确失败）。

use tokio::sync::mpsc as tokio_mpsc;

use crate::context::organizer::ContextOrganizer;
use crate::core::command::Command;
use crate::model::TokenUsage;
use crate::turn_context::TurnContext;

use super::command::Deferred;
use super::compression::{CommandPolicy, CompressionInterrupt, ContextCompression};

/// 请求前准备的结果。
pub(super) enum RequestPreparation {
    /// 无需压缩或压缩已应用，可以发起请求。
    Ready,
    /// 准备期间到达命令：中止压缩并交还执行驱动处理（调用方设 deferred）。
    Interrupted(Deferred),
}

/// 上下文溢出恢复的结果。
pub(super) enum ContextRecovery {
    /// 已压缩（或仍有可压缩空间），可以重试请求。
    Retriable,
    /// 没有可压缩的较早历史，重试也不会成功：调用方应明确失败。
    Exhausted,
    /// 恢复期间到达命令：交还执行驱动处理。
    Interrupted(Deferred),
}

/// 请求前上下文准备：观测压力超过阈值时先压缩（ALR-303）。
pub(super) async fn prepare_before_request(
    ctx: &mut TurnContext,
    accumulated_usage: &mut TokenUsage,
    organizer: &ContextOrganizer,
    observed_tokens: usize,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
) -> RequestPreparation {
    if !organizer.needs_compression(observed_tokens) {
        return RequestPreparation::Ready;
    }
    let mut compression = ContextCompression::auto(ctx, organizer, observed_tokens);
    match compression.run(ctx, cmd_rx, CommandPolicy::Relay).await {
        Ok(result) => {
            compression.complete(ctx, result, Some(accumulated_usage));
            RequestPreparation::Ready
        }
        Err(interrupt) => RequestPreparation::Interrupted(interrupted_deferred(interrupt)),
    }
}

/// 上下文溢出错误恢复：强制压缩后重试；无较早历史可压时明确失败（ALR-304）。
pub(super) async fn recover_context_overflow(
    ctx: &mut TurnContext,
    accumulated_usage: &mut TokenUsage,
    organizer: &ContextOrganizer,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
) -> ContextRecovery {
    let mut compression = ContextCompression::forced(ctx, organizer);
    match compression.run(ctx, cmd_rx, CommandPolicy::Relay).await {
        Ok(result) => {
            // 边界未推进的压缩（无可压缩历史）重试无意义。
            let previous = ctx.session.summary_up_to;
            compression.complete(ctx, result, Some(accumulated_usage));
            if ctx.session.summary_up_to > previous {
                ContextRecovery::Retriable
            } else {
                ContextRecovery::Exhausted
            }
        }
        Err(interrupt) => ContextRecovery::Interrupted(interrupted_deferred(interrupt)),
    }
}

fn interrupted_deferred(interrupt: CompressionInterrupt) -> Deferred {
    match interrupt {
        CompressionInterrupt::Command(command) => Deferred::Command(command),
        CompressionInterrupt::Closed => Deferred::Closed,
    }
}
