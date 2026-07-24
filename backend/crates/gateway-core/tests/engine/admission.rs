use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_core::engine::admission::{
    ClientAdmissionDecision, ClientAdmissionError, ClientAdmissionPort, ClientAdmissionRecovery,
    ClientAdmissionRecoveryPort, ClientAdmissionRequest, ClientAdmissionRestoreResult,
    RecentAdmissionFact, RunningAdmissionFact, restore_client_admission_startup,
};
use gateway_core::engine::{
    AttemptRecord, ExecutionStore, IntermediateFailure, ModelRequestFinalization, ModelRequestId,
    NewModelRequest, RecoveryReport, UpstreamSendState,
};
use gateway_core::error::{StoreError, StoreErrorKind};
use gateway_core::policy::ClientApiKeyId;

#[test]
fn client_admission_startup_recovery_should_preserve_order_and_exact_facts() {
    futures::executor::block_on(async {
        let now = SystemTime::now();
        let request = ModelRequestId::new("req_recovery").expect("request id");
        let recovery = ClientAdmissionRecovery {
            client_api_key_id: ClientApiKeyId::new("key_recovery").expect("key id"),
            recent_requests: vec![RecentAdmissionFact {
                model_request_id: request.clone(),
                started_at: now - Duration::from_secs(2),
            }],
            running_requests: vec![RunningAdmissionFact {
                model_request_id: request,
                expires_at: now + Duration::from_secs(30),
            }],
        };
        let recoveries = ScriptedRecoveries::new(Ok(vec![recovery.clone()]));
        let admissions = RecordingAdmissions::default();
        let report = restore_client_admission_startup(
            &RecoveryStore::success(2),
            &recoveries,
            &admissions,
            now,
        )
        .await
        .expect("startup recovery");

        assert_eq!(report.expired_model_requests, 2);
        assert_eq!(report.restored_clients, 1);
        assert_eq!(
            admissions.restored.lock().expect("restored").as_slice(),
            &[recovery]
        );
    });
}

#[test]
fn client_admission_startup_recovery_should_fail_closed_at_each_boundary() {
    futures::executor::block_on(async {
        let now = SystemTime::now();
        assert!(
            restore_client_admission_startup(
                &RecoveryStore::failure(),
                &ScriptedRecoveries::new(Ok(Vec::new())),
                &RecordingAdmissions::default(),
                now,
            )
            .await
            .is_err()
        );
        assert!(
            restore_client_admission_startup(
                &RecoveryStore::success(0),
                &ScriptedRecoveries::new(Err(ClientAdmissionError)),
                &RecordingAdmissions::default(),
                now,
            )
            .await
            .is_err()
        );
        let admissions = RecordingAdmissions {
            fail_restore: true,
            ..RecordingAdmissions::default()
        };
        assert!(
            restore_client_admission_startup(
                &RecoveryStore::success(0),
                &ScriptedRecoveries::new(Ok(vec![empty_recovery()])),
                &admissions,
                now,
            )
            .await
            .is_err()
        );
    });
}

fn empty_recovery() -> ClientAdmissionRecovery {
    ClientAdmissionRecovery {
        client_api_key_id: ClientApiKeyId::new("key_empty").expect("key id"),
        recent_requests: Vec::new(),
        running_requests: Vec::new(),
    }
}

struct RecoveryStore(Result<RecoveryReport, StoreError>);

impl RecoveryStore {
    fn success(requests: u64) -> Self {
        Self(Ok(RecoveryReport { requests }))
    }

    fn failure() -> Self {
        Self(Err(StoreError::new(StoreErrorKind::Unavailable)))
    }
}

#[async_trait]
impl ExecutionStore for RecoveryStore {
    async fn create_model_request(&self, _: NewModelRequest) -> Result<(), StoreError> {
        unreachable!()
    }
    async fn record_attempt(&self, _: AttemptRecord) -> Result<(), StoreError> {
        unreachable!()
    }
    async fn mark_send_state(
        &self,
        _: &ModelRequestId,
        _: UpstreamSendState,
    ) -> Result<(), StoreError> {
        unreachable!()
    }
    async fn mark_downstream_committed(
        &self,
        _: &ModelRequestId,
        _: SystemTime,
        _: Option<u16>,
    ) -> Result<(), StoreError> {
        unreachable!()
    }
    async fn record_client_status(&self, _: &ModelRequestId, _: u16) -> Result<(), StoreError> {
        unreachable!()
    }
    async fn record_intermediate_failure(&self, _: IntermediateFailure) -> Result<(), StoreError> {
        unreachable!()
    }
    async fn finalize_model_request(&self, _: ModelRequestFinalization) -> Result<(), StoreError> {
        unreachable!()
    }
    async fn recover_expired(&self, _: SystemTime) -> Result<RecoveryReport, StoreError> {
        self.0.clone()
    }
}

struct ScriptedRecoveries(
    Mutex<VecDeque<Result<Vec<ClientAdmissionRecovery>, ClientAdmissionError>>>,
);

impl ScriptedRecoveries {
    fn new(result: Result<Vec<ClientAdmissionRecovery>, ClientAdmissionError>) -> Self {
        Self(Mutex::new(VecDeque::from([result])))
    }
}

impl ClientAdmissionRecoveryPort for ScriptedRecoveries {
    fn load_recovery(
        &self,
        _: SystemTime,
    ) -> BoxFuture<'_, Result<Vec<ClientAdmissionRecovery>, ClientAdmissionError>> {
        Box::pin(async move {
            self.0
                .lock()
                .expect("recovery script")
                .pop_front()
                .expect("script")
        })
    }
}

#[derive(Default)]
struct RecordingAdmissions {
    restored: Mutex<Vec<ClientAdmissionRecovery>>,
    fail_restore: bool,
}

impl ClientAdmissionPort for RecordingAdmissions {
    fn admit(
        &self,
        _: ClientAdmissionRequest,
    ) -> BoxFuture<'_, Result<ClientAdmissionDecision, ClientAdmissionError>> {
        unreachable!()
    }
    fn release<'a>(
        &'a self,
        _: &'a ClientApiKeyId,
        _: &'a ModelRequestId,
    ) -> BoxFuture<'a, Result<bool, ClientAdmissionError>> {
        unreachable!()
    }
    fn restore(
        &self,
        recovery: ClientAdmissionRecovery,
    ) -> BoxFuture<'_, Result<ClientAdmissionRestoreResult, ClientAdmissionError>> {
        Box::pin(async move {
            if self.fail_restore {
                return Err(ClientAdmissionError);
            }
            self.restored.lock().expect("restored").push(recovery);
            Ok(ClientAdmissionRestoreResult {
                restored_recent_requests: 1,
                restored_running_requests: 1,
            })
        })
    }
}
