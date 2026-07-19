use gateway_api::admin::openai::{
    CompleteOAuthAuthorizationRequest, CredentialDetailsQuery, CredentialMutationRequest,
    ImportCredentialsDocumentRequest, ListCredentialsQuery, RotateCredentialRequest,
    StartOAuthAuthorizationRequest,
};

fn compact_jwt() -> String {
    "header.payload.signature".to_owned()
}

#[test]
fn document_import_accepts_provider_owned_object() {
    let request: ImportCredentialsDocumentRequest = serde_json::from_value(serde_json::json!({
        "expectedConfigRevision": 7,
        "providerInstanceId": "inst_openai",
        "document": {"accounts": [{"token": compact_jwt()}]}
    }))
    .expect("deserialize Provider import document");

    assert_eq!(request.validate(), Ok(()));
}

#[test]
fn document_import_rejects_unknown_outer_fields() {
    let result = serde_json::from_value::<ImportCredentialsDocumentRequest>(serde_json::json!({
        "expectedConfigRevision": 7,
        "providerInstanceId": "inst_openai",
        "document": {"accounts": []},
        "maxConcurrency": 2
    }));

    assert!(result.is_err());
}

#[test]
fn document_import_rejects_non_object_document() {
    let array: ImportCredentialsDocumentRequest = serde_json::from_value(serde_json::json!({
        "expectedConfigRevision": 7,
        "providerInstanceId": "inst_openai",
        "document": []
    }))
    .expect("deserialize array document");

    assert_eq!(array.validate().unwrap_err().field(), "document");
}

#[test]
fn document_import_keeps_provider_document_opaque_to_api() {
    let request: ImportCredentialsDocumentRequest = serde_json::from_value(serde_json::json!({
        "expectedConfigRevision": 7,
        "providerInstanceId": "inst_openai",
        "document": {
            "providerOwnedUnknownField": {"nested": [1, 2, 3]},
            "accounts": [{"credentials": {"access_token": "provider-validates-this"}}]
        }
    }))
    .expect("deserialize opaque Provider document");

    assert_eq!(request.validate(), Ok(()));
}

#[test]
fn list_query_should_validate_status_pagination_and_reserved_ids() {
    let valid: ListCredentialsQuery = serde_json::from_value(serde_json::json!({
        "providerInstanceId": "inst_openai",
        "availability": "cooldown",
        "enabled": false,
        "cursor": "cursor",
        "limit": 200
    }))
    .expect("deserialize list query");
    let invalid_status: ListCredentialsQuery = serde_json::from_value(serde_json::json!({
        "availability": "refreshing"
    }))
    .expect("deserialize invalid status query");
    let invalid_limit: ListCredentialsQuery = serde_json::from_value(serde_json::json!({
        "limit": 201
    }))
    .expect("deserialize invalid page size query");

    assert_eq!(valid.validate(), Ok(()));
    assert_eq!(
        invalid_status.validate().unwrap_err().field(),
        "availability"
    );
    assert_eq!(invalid_limit.validate().unwrap_err().field(), "limit");
}

#[test]
fn detail_and_mutation_should_reject_missing_invalid_or_overflowing_identity() {
    let detail = CredentialDetailsQuery {
        credential_id: "__reserved__".to_owned(),
    };
    let mutation = CredentialMutationRequest {
        credential_id: "cred_codex".to_owned(),
        expected_config_revision: 0,
    };
    let rotation: RotateCredentialRequest = serde_json::from_value(serde_json::json!({
        "credentialId": "cred_codex",
        "expectedConfigRevision": 1,
        "expectedCredentialRevision": u64::MAX,
        "accessToken": compact_jwt()
    }))
    .expect("deserialize overflowing revision");

    assert_eq!(detail.validate().unwrap_err().field(), "credentialId");
    assert_eq!(
        mutation.validate().unwrap_err().field(),
        "expectedConfigRevision"
    );
    assert_eq!(
        rotation.validate().unwrap_err().field(),
        "expectedCredentialRevision"
    );
}

#[test]
fn oauth_wire_should_keep_flow_id_and_callback_in_post_body_only() {
    let start: StartOAuthAuthorizationRequest = serde_json::from_value(serde_json::json!({
        "expectedConfigRevision": 7,
        "providerInstanceId": "inst_openai",
        "name": "OAuth browser flow",
        "credentialId": "acct_existing",
        "expectedCredentialRevision": 9
    }))
    .expect("start wire");
    let complete: CompleteOAuthAuthorizationRequest = serde_json::from_value(serde_json::json!({
        "flowId": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "callbackUrl": "http://localhost:1455/auth/callback?code=code&state=state"
    }))
    .expect("complete wire");
    let client_state =
        serde_json::from_value::<StartOAuthAuthorizationRequest>(serde_json::json!({
            "expectedConfigRevision": 7,
            "providerInstanceId": "inst_openai",
            "name": "OAuth browser flow",
            "state": "client-controlled"
        }))
        .expect_err("state must remain server-side");

    assert_eq!(start.validate(), Ok(()));
    assert_eq!(complete.validate(), Ok(()));
    assert!(client_state.to_string().contains("unknown field `state`"));
}

#[test]
fn oauth_reauthorization_requires_account_and_revision_together() {
    let missing_revision: StartOAuthAuthorizationRequest =
        serde_json::from_value(serde_json::json!({
            "expectedConfigRevision": 7,
            "providerInstanceId": "inst_openai",
            "name": "OAuth browser flow",
            "credentialId": "acct_existing"
        }))
        .expect("deserialize incomplete reauthorization");

    assert_eq!(
        missing_revision.validate().unwrap_err().field(),
        "reauthorization"
    );
}
