use std::sync::Mutex;

use async_trait::async_trait;

use gateway_admin::{
    model::system::{
        SystemOperationAccepted, SystemOperationState, SystemOperationStatus, SystemUpdateDetail,
        SystemUpdateStatus, SystemVersion,
    },
    ports::system::{SystemOperationError, SystemOperations, SystemUpdateEventStream},
};

#[derive(Default)]
struct RecordingSystemOperations {
    target: Mutex<Option<Option<String>>>,
}

#[async_trait]
impl SystemOperations for RecordingSystemOperations {
    async fn version(&self) -> Result<SystemVersion, SystemOperationError> {
        Ok(SystemVersion {
            version: "1.0.0".to_owned(),
            git_sha: "unknown".to_owned(),
            build_time: "unknown".to_owned(),
            deployment_mode: "source".to_owned(),
            update_channel: "stable".to_owned(),
            latest_version: "1.0.0".to_owned(),
            has_update: false,
            update_cached: false,
            update_warning: None,
        })
    }

    async fn update_detail(&self, _: bool) -> Result<SystemUpdateDetail, SystemOperationError> {
        Ok(SystemUpdateDetail {
            current_version: "1.0.0".to_owned(),
            latest_version: "1.0.0".to_owned(),
            has_update: false,
            deployment_mode: "source".to_owned(),
            build_type: "source".to_owned(),
            release_url: None,
            notes: None,
            cached: false,
            update_supported: false,
            unsupported_reason: Some("source build".to_owned()),
            warning: None,
        })
    }

    fn update_events(&self) -> SystemUpdateEventStream {
        Box::pin(futures::stream::empty())
    }

    async fn perform_update(
        &self,
        target_version: Option<String>,
    ) -> Result<SystemOperationAccepted, SystemOperationError> {
        *self.target.lock().expect("target") = Some(target_version.clone());
        Ok(SystemOperationAccepted::Update {
            operation_id: "operation-update".to_owned(),
            deployment_mode: "source".to_owned(),
            message: "accepted".to_owned(),
            need_restart: true,
            target_version: target_version.unwrap_or_else(|| "latest".to_owned()),
        })
    }

    async fn update_status(&self) -> Result<SystemUpdateStatus, SystemOperationError> {
        Ok(SystemUpdateStatus {
            previous_version: None,
            current_version: Some("1.0.0".to_owned()),
            operation: SystemOperationState {
                operation_id: None,
                kind: None,
                status: SystemOperationStatus::Idle,
                target_version: None,
                message: None,
                error: None,
                started_at: None,
                finished_at: None,
            },
        })
    }

    async fn rollback(&self) -> Result<SystemOperationAccepted, SystemOperationError> {
        Ok(SystemOperationAccepted::Rollback {
            operation_id: "operation-rollback".to_owned(),
            message: "accepted".to_owned(),
            need_restart: true,
        })
    }

    async fn restart(&self) -> Result<SystemOperationAccepted, SystemOperationError> {
        Ok(SystemOperationAccepted::Restart {
            operation_id: "operation-restart".to_owned(),
            message: "accepted".to_owned(),
        })
    }
}

#[tokio::test]
async fn system_update_should_normalize_blank_target_to_latest() {
    let operations = std::sync::Arc::new(RecordingSystemOperations::default());
    let services = super::AdminHarness::new()
        .system(operations.clone())
        .build()
        .await;
    services
        .system()
        .perform_update(Some("   ".to_owned()))
        .await
        .expect("perform update");

    assert_eq!(*operations.target.lock().expect("target"), Some(None));
}
