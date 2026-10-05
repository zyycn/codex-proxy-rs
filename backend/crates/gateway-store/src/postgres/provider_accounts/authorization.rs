//! 完成回执与账号、审计共享提交边界；Redis claim 只负责上游准备阶段的互斥

use gateway_admin::model::provider_credentials::{
    AuthorizationCommitResult, AuthorizationMutationTarget, AuthorizationReceiptKey,
    CredentialMutationResult,
};
use serde::{Deserialize, Serialize};

use super::{admin_adapter, repository, *};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAccount {
    account_id: String,
    credential_revision: Option<u64>,
}

impl PgAdminAccountStore {
    pub(super) async fn load_authorization_receipt(
        &self,
        key: &AuthorizationReceiptKey,
    ) -> AdminStoreResult<Option<CredentialMutationResult>> {
        load_receipt(&self.pool, key).await
    }

    pub(super) async fn commit_authorization_once(
        &self,
        command: AuthorizationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<AuthorizationCommitResult> {
        if command.key.provider_kind() != command.pending.provider_kind()
            || !command.key.matches_context(context)
            || !command.pending.owner_binding().matches_context(context)
        {
            return Err(invalid_receipt());
        }
        let mut transaction = self.pool.begin().await.map_err(|_| unavailable_receipt())?;
        let outcome = commit_authorization_in_transaction(&mut transaction, command, context).await;
        match outcome {
            Ok(outcome) => {
                transaction
                    .commit()
                    .await
                    .map_err(|_| unavailable_receipt())?;
                Ok(outcome)
            }
            Err(error) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| unavailable_receipt())?;
                Err(error)
            }
        }
    }
}

async fn commit_authorization_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    command: AuthorizationCommit,
    context: &MutationContext,
) -> AdminStoreResult<AuthorizationCommitResult> {
    let key = &command.key;
    // 同一 flow 的竞争者先等待事务结果；Redis 租约过期不能造成第二次账号提交
    let lock = format!(
        "authorization:{}:{}",
        key.provider_kind(),
        key.flow_digest()
    );
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 814037))")
        .bind(lock)
        .execute(&mut **transaction)
        .await
        .map_err(|_| unavailable_receipt())?;
    if let Some(result) = load_receipt(&mut **transaction, key).await? {
        return Ok(AuthorizationCommitResult {
            result,
            newly_committed: false,
        });
    }
    let result = match (command.pending.target(), command.credential) {
        (
            AuthorizationMutationTarget::Create { .. },
            AuthorizationCredentialCommit::Create(credential),
        ) => {
            if &credential.provider_kind != key.provider_kind() {
                return Err(invalid_receipt());
            }
            let prepared = admin_adapter::prepare_import(
                PreparedCredentialImport {
                    provider_kind: key.provider_kind().clone(),
                    credentials: vec![*credential],
                },
                command.settings,
                context,
                gateway_admin::model::audit::MutationAuditOperation::ProviderAccountAuthorize,
                command
                    .pending
                    .outbound_proxy_id()
                    .zip(command.pending.outbound_proxy())
                    .map(
                        |(id, proxy)| gateway_admin::model::proxies::ImportProxyBinding {
                            id: id.into(),
                            proxy: proxy.clone(),
                        },
                    ),
            )?;
            let result = repository::import_provider_accounts_in_transaction(transaction, prepared)
                .await
                .map_err(|error| admin_store_error(ENTITY, error))?;
            admin_adapter::authorization_import_result(result)?
        }
        (
            AuthorizationMutationTarget::Reauthorize { account_id },
            AuthorizationCredentialCommit::Reauthorize(prepared),
        ) => {
            if command.settings.is_some()
                || &prepared.account_id != account_id
                || &prepared.provider_kind != key.provider_kind()
            {
                return Err(invalid_receipt());
            }
            let prepared = admin_adapter::prepare_rotation(
                *prepared,
                None,
                context,
                gateway_admin::model::audit::MutationAuditOperation::ProviderAccountReauthorize,
            )?;
            let result =
                repository::rotate_provider_account_admin_in_transaction(transaction, prepared)
                    .await
                    .map_err(|error| admin_store_error(ENTITY, error))?;
            admin_adapter::rotation_result(result, account_id.clone())?
        }
        _ => return Err(invalid_receipt()),
    };
    let accounts = [StoredAccount {
        account_id: result.account_id.to_string(),
        credential_revision: result.credential_revision.map(|revision| revision.get()),
    }];
    sqlx::query("insert into authorization_receipts(provider_kind, flow_digest, owner_digest, config_revision, accounts_json) values ($1,$2,$3,$4,$5)")
            .bind(key.provider_kind().as_str()).bind(key.flow_digest()).bind(key.owner_digest())
            .bind(i64::try_from(result.config_revision.get()).map_err(|_| invalid_receipt())?)
            .bind(serde_json::to_value(accounts).map_err(|_| invalid_receipt())?)
            .execute(&mut **transaction).await.map_err(|_| unavailable_receipt())?;
    // 保留期长于 OAuth pending 上限；每次提交有界回收，避免引入第二套维护任务
    sqlx::query("delete from authorization_receipts where ctid in (select ctid from authorization_receipts where expires_at <= now() order by expires_at limit 128 for update skip locked)")
            .execute(&mut **transaction).await.map_err(|_| unavailable_receipt())?;
    Ok(AuthorizationCommitResult {
        result,
        newly_committed: true,
    })
}

async fn load_receipt<'a>(
    executor: impl sqlx::PgExecutor<'a>,
    key: &AuthorizationReceiptKey,
) -> AdminStoreResult<Option<CredentialMutationResult>> {
    let row: Option<(String, i64, serde_json::Value)> = sqlx::query_as("select owner_digest, config_revision, accounts_json from authorization_receipts where provider_kind=$1 and flow_digest=$2 and expires_at>now()")
        .bind(key.provider_kind().as_str()).bind(key.flow_digest()).fetch_optional(executor).await.map_err(|_| unavailable_receipt())?;
    let Some((owner, revision, accounts)) = row else {
        return Ok(None);
    };
    if owner != key.owner_digest() {
        return Err(invalid_receipt());
    }
    let [account]: [StoredAccount; 1] =
        serde_json::from_value(accounts).map_err(|_| unavailable_receipt())?;
    Ok(Some(CredentialMutationResult {
        config_revision: AdminRevision::new(
            u64::try_from(revision).map_err(|_| unavailable_receipt())?,
        )
        .map_err(|_| unavailable_receipt())?,
        account_id: CoreProviderAccountId::new(account.account_id)
            .map_err(|_| unavailable_receipt())?,
        credential_revision: account
            .credential_revision
            .map(|revision| AdminRevision::new(revision).map_err(|_| unavailable_receipt()))
            .transpose()?,
    }))
}

fn invalid_receipt() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Invalid,
        ENTITY,
        "invalid authorization receipt binding",
    )
}
fn unavailable_receipt() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        ENTITY,
        "authorization receipt unavailable",
    )
}
