use provider_xai::{VerificationEvidence, VerificationMethod};

#[test]
fn verification_evidence_should_redact_verified_subject() {
    let evidence = VerificationEvidence::id_token("verified-subject".to_owned());
    let debug = format!("{evidence:?}");

    assert_eq!(
        (evidence.method(), evidence.subject(), debug),
        (
            VerificationMethod::IdToken,
            "verified-subject",
            "VerificationEvidence { method: IdToken, subject: \"[REDACTED]\" }".to_owned(),
        )
    );
}
