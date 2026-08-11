//! 通用存根后端：用于尚未实现原生无障碍能力的平台。
//!
//! 所有能力返回明确的能力不足结果，运行时不影响宿主启动。
//! `status` 报告无图形会话/能力不可用，其余操作返回对应业务错误。

use async_trait::async_trait;

use super::{ActionResult, Backend, FindInfo, SnapshotInfo, StatusInfo, WaitResult};
use tiangong_plugin_computer_use_protocol::{DesktopError, DesktopResult, Platform};

pub struct StubBackend {
    platform: Platform,
}

impl StubBackend {
    /// 构造一个返回“平台不支持”的存根后端。
    pub fn unsupported(platform: Platform) -> Self {
        Self { platform }
    }
}

fn unsupported(platform: Platform) -> DesktopError {
    DesktopError::UnsupportedPlatform {
        platform: format!("{platform:?}").to_lowercase(),
    }
}

#[async_trait]
impl Backend for StubBackend {
    fn platform(&self) -> Platform {
        self.platform
    }

    async fn status(&self) -> DesktopResult<StatusInfo> {
        // 存根平台无图形会话感知，统一报告会话不可用 + 能力不可用。
        DesktopResult::Err(DesktopError::BackendUnavailable {
            reason: format!("{:?} 平台尚未接入原生无障碍后端", self.platform),
        })
    }

    async fn list_windows(
        &self,
        _req: &tiangong_plugin_computer_use_protocol::ops::ListWindowsRequest,
    ) -> DesktopResult<tiangong_plugin_computer_use_protocol::ListWindowsResponse> {
        DesktopResult::Err(unsupported(self.platform))
    }

    async fn snapshot(
        &self,
        _req: &tiangong_plugin_computer_use_protocol::ops::SnapshotRequest,
    ) -> DesktopResult<SnapshotInfo> {
        DesktopResult::Err(unsupported(self.platform))
    }

    async fn find(
        &self,
        _req: &tiangong_plugin_computer_use_protocol::ops::FindRequest,
    ) -> DesktopResult<FindInfo> {
        DesktopResult::Err(unsupported(self.platform))
    }

    async fn action(
        &self,
        _req: &tiangong_plugin_computer_use_protocol::ops::ActionRequest,
    ) -> DesktopResult<ActionResult> {
        DesktopResult::Err(unsupported(self.platform))
    }

    async fn wait(
        &self,
        _req: &tiangong_plugin_computer_use_protocol::ops::WaitRequest,
    ) -> DesktopResult<WaitResult> {
        DesktopResult::Err(unsupported(self.platform))
    }
}
