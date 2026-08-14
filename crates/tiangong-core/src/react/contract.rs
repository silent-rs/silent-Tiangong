//! 工具义务与候选完成门控（ALR-003/006/007/008/009/307）。
//!
//! [`TaskContract`] 只记录程序可以核验的义务与证据，不调用第二个自由文本
//! 模型判断"任务是否完成"，也不根据回答长度或模型文字中的"已完成"判断
//! （这不是 Summary 的改名版本）。模型无工具调用的响应只形成候选完成，
//! 由 [`TaskContract`] 同步门控：无未满足义务才允许提交。
//!
//! 第一批义务只覆盖入口显式声明的高置信度场景：用户消息携带的附件
//! （`AssetReference` + 宿主 `ModelInstruction`）。义务满足的证据是
//! 真实成功的工具结果；失败、拒绝或取消不能满足义务。

use crate::turn_context::TurnContext;
use tiangong_types::ContentBlock;

/// 工具协议修复预算（ALR-305）：只处理"明确需要工具但模型漏发工具"的
/// 协议违约，不参与一般完成度判断，因此很小且可观测。
pub(super) const MAX_TOOL_PROTOCOL_REPAIRS: u8 = 2;

/// 纯文本响应经候选完成门控后的去向。
pub(super) enum ReactTextDisposition {
    /// 门控通过：作为最终答复提交（PendingFinish）。
    Complete,
    /// 无效输出或存在未满足义务，预算内：注入修复提示后继续请求模型
    ///（下次请求附加 Provider 工具约束）。
    RepairRequired { reason: String },
    /// 修复预算耗尽：明确失败，不把未验证文本发布为成功。
    Exhausted { reason: String },
}

/// 单项工具义务。第一批只有入口声明的附件读取。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ToolObligation {
    /// 用户消息携带文件/媒体附件，内容不直接进入模型请求，必须实际调用
    /// 工具读取后才允许完成。
    ReadAttachment { asset_name: String },
}

impl ToolObligation {
    fn describe(&self) -> String {
        match self {
            Self::ReadAttachment { asset_name } => format!("读取附件 {asset_name}"),
        }
    }
}

/// 义务状态。失败的工具结果让义务保持 [`ObligationStatus::Pending`]，
/// 等待模型重试或最终失败——没有独立的 Failed 终态（不可虚报，ALR-307）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ObligationStatus {
    /// 尚未取得成功证据。
    Pending,
    /// 已由真实成功的工具结果满足。
    Satisfied,
}

struct ObligationEntry {
    obligation: ToolObligation,
    status: ObligationStatus,
}

/// 当前任务的工具义务集合与协议修复计数。
///
/// 义务在 turn 开始（或 steer 注入新意图）时从锚点用户消息构建；
/// 普通解释、写作和闲聊没有工具义务，纯文本可直接完成（ALR-006 反向约束）。
pub(super) struct TaskContract {
    obligations: Vec<ObligationEntry>,
    protocol_repairs: u8,
}

impl TaskContract {
    /// 从当前锚点用户消息构建义务（入口显式声明：附件块）。
    pub(super) fn from_session_anchor(ctx: &TurnContext) -> Self {
        let mut obligations = Vec::new();
        if let Some(index) = ctx.session.latest_user_message_index() {
            for block in &ctx.session.messages[index].content {
                if let ContentBlock::AssetReference { asset } = block {
                    obligations.push(ObligationEntry {
                        obligation: ToolObligation::ReadAttachment {
                            asset_name: asset.original_name.clone(),
                        },
                        status: ObligationStatus::Pending,
                    });
                }
            }
        }
        Self {
            obligations,
            protocol_repairs: 0,
        }
    }

    #[cfg(test)]
    pub(super) fn empty() -> Self {
        Self {
            obligations: Vec::new(),
            protocol_repairs: 0,
        }
    }

    /// 是否存在未满足义务（请求前工具约束与门控共用，ALR-007）。
    pub(super) fn has_unsatisfied(&self) -> bool {
        self.obligations
            .iter()
            .any(|entry| entry.status == ObligationStatus::Pending)
    }

    /// 已使用的协议修复次数（日志与诊断用）。
    pub(super) fn protocol_repairs(&self) -> u8 {
        self.protocol_repairs
    }

    /// 请求前工具约束：存在未满足义务时要求模型必须调用工具。
    pub(super) fn tool_constraint(&self) -> Option<crate::model::ToolChoice> {
        self.has_unsatisfied()
            .then_some(crate::model::ToolChoice::Any)
    }

    /// 工具结果落库后更新义务：只有成功结果可满足义务（ALR-307）。
    pub(super) fn record_tool_result(&mut self, ok: bool) {
        if !ok {
            return;
        }
        for entry in &mut self.obligations {
            if entry.status == ObligationStatus::Pending {
                entry.status = ObligationStatus::Satisfied;
            }
        }
    }

    /// 候选完成门控：纯文本响应是否可以提交为最终答复。
    ///
    /// 判定完全同步、确定性：空回复是无效输出；存在未满足义务时模型文字
    /// 不构成证据。预算内的违约进入修复，耗尽后明确失败。
    pub(super) fn gate_text(&self, text: &str) -> ReactTextDisposition {
        let reason = if let Some(missing) = self.unsatisfied_summary() {
            missing
        } else if text.trim().is_empty() {
            "模型未产生有效回复".to_string()
        } else {
            return ReactTextDisposition::Complete;
        };
        self.gate_repair(reason)
    }

    /// 合成工具调用占位符不是有效输出：同样进入修复路径。
    pub(super) fn gate_placeholder(&self) -> ReactTextDisposition {
        self.gate_repair("模型返回合成占位符，需要真实输出或工具调用".to_string())
    }

    fn gate_repair(&self, reason: String) -> ReactTextDisposition {
        if self.protocol_repairs < MAX_TOOL_PROTOCOL_REPAIRS {
            ReactTextDisposition::RepairRequired { reason }
        } else {
            ReactTextDisposition::Exhausted {
                reason: format!(
                    "当前任务需要实际执行工具，但模型连续未提供有效工具调用，未能确认任务完成（{reason}）"
                ),
            }
        }
    }

    /// 发起一次协议修复：递增计数并返回注入给模型的明确指令（ALR-008）。
    pub(super) fn begin_repair(&mut self) -> &'static str {
        self.protocol_repairs += 1;
        "上一次回复缺少必需的工具调用。请立即返回读取附件所需的 tool call，不要只输出文字说明。"
    }

    /// 未满足义务清单（修复提示与失败信息用）。
    fn unsatisfied_summary(&self) -> Option<String> {
        let missing: Vec<String> = self
            .obligations
            .iter()
            .filter(|entry| entry.status == ObligationStatus::Pending)
            .map(|entry| entry.obligation.describe())
            .collect();
        (!missing.is_empty()).then(|| format!("仍有未完成的工具义务：{}", missing.join("、")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract_with_attachment() -> TaskContract {
        TaskContract {
            obligations: vec![ObligationEntry {
                obligation: ToolObligation::ReadAttachment {
                    asset_name: "report.pdf".to_string(),
                },
                status: ObligationStatus::Pending,
            }],
            protocol_repairs: 0,
        }
    }

    #[test]
    fn plain_text_completes_without_obligations() {
        let contract = TaskContract::empty();
        assert!(matches!(
            contract.gate_text("贪心算法是一种……"),
            ReactTextDisposition::Complete
        ));
        assert!(contract.tool_constraint().is_none());
    }

    #[test]
    fn unsatisfied_obligation_rejects_text_and_repairs_bounded() {
        let mut contract = contract_with_attachment();
        assert!(
            contract.tool_constraint().is_some(),
            "未满足义务时应要求工具"
        );

        // 纯文本被拒绝：预算内进入修复。
        assert!(matches!(
            contract.gate_text("我已经读取并分析完了。"),
            ReactTextDisposition::RepairRequired { .. }
        ));
        contract.begin_repair();

        assert!(matches!(
            contract.gate_text("再次声称完成"),
            ReactTextDisposition::RepairRequired { .. }
        ));
        contract.begin_repair();

        // 预算耗尽：明确失败，不虚报成功。
        assert!(matches!(
            contract.gate_text("第三次声称完成"),
            ReactTextDisposition::Exhausted { .. }
        ));
    }

    #[test]
    fn only_successful_tool_result_satisfies_obligation() {
        let mut contract = contract_with_attachment();
        contract.record_tool_result(false);
        assert!(contract.has_unsatisfied(), "工具失败不能满足义务");
        contract.record_tool_result(true);
        assert!(!contract.has_unsatisfied(), "成功结果满足义务");
        assert!(matches!(
            contract.gate_text("附件分析完成。"),
            ReactTextDisposition::Complete
        ));
    }

    #[test]
    fn empty_text_is_invalid_output_even_without_obligations() {
        let contract = TaskContract::empty();
        assert!(matches!(
            contract.gate_text("   "),
            ReactTextDisposition::RepairRequired { .. }
        ));
    }
}
