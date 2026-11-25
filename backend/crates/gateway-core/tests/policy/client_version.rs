//! 对应生产职责的行为测试

use super::*;

#[test]
fn client_version_should_require_strict_semver() {
    assert!(CodexClientVersion::parse("0.152.0").is_ok());
    assert!(CodexClientVersion::parse("v0.152.0").is_err());
    assert!(CodexClientVersion::parse("0.152").is_err());
    assert!(CodexClientVersion::parse(" 0.152.0").is_err());
    assert!(CodexClientVersion::parse("18446744073709551616.0.0").is_err());
}

#[test]
fn min_client_versions_should_reject_missing_and_old_recognized_clients() {
    let min = CodexClientVersion::parse("0.40.0").expect("min version");
    let policy = CodexClientMinVersions::new(None, Some(min.clone()));
    let old = CodexClientVersion::parse("0.39.0").expect("old version");
    let current = CodexClientVersion::parse("0.40.0").expect("current version");

    assert_eq!(
        policy.enforce(CodexClientKind::Cli, None),
        Err(ClientVersionRejection::Unavailable {
            kind: CodexClientKind::Cli,
            min: min.clone(),
        })
    );
    assert_eq!(
        policy.enforce(CodexClientKind::Cli, Some(&old)),
        Err(ClientVersionRejection::TooOld {
            kind: CodexClientKind::Cli,
            current: old,
            min,
        })
    );
    assert_eq!(policy.enforce(CodexClientKind::Cli, Some(&current)), Ok(()));
    assert_eq!(policy.enforce(CodexClientKind::Desktop, None), Ok(()));
}

#[test]
fn min_client_versions_should_follow_semver_precedence() {
    for (minimum, current, allowed) in [
        ("1.0.0", "1.0.0-rc.1", false),
        ("1.0.0", "1.0.0", true),
        ("1.0.0", "1.0.1", true),
        ("1.0.0-rc.2", "1.0.0-rc.1", false),
        ("1.0.0-rc.2", "1.0.0-rc.10", true),
        ("1.0.0+build.2", "1.0.0", true),
        ("1.0.0", "1.0.0+build.2", true),
        ("1.0.0+build.2", "1.0.0+build.1", true),
        ("1.0.0-rc.2+build.2", "1.0.0-rc.2+build.1", true),
        ("1.0.0+build.1", "1.0.0-rc.1+build.9", false),
        ("1.0.1+build.1", "1.0.0+build.9", false),
    ] {
        let minimum_version = CodexClientVersion::parse(minimum).unwrap();
        let current_version = CodexClientVersion::parse(current).unwrap();
        let policy =
            CodexClientMinVersions::new(Some(minimum_version.clone()), Some(minimum_version));
        for kind in [CodexClientKind::Cli, CodexClientKind::Desktop] {
            assert_eq!(
                policy.enforce(kind, Some(&current_version)).is_ok(),
                allowed,
                "{kind:?}: {current} >= {minimum}"
            );
        }
    }
}
