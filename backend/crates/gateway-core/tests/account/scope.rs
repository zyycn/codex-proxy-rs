//! 互联授权与分组、模型和请求局部范围的交集

use super::*;
use gateway_core::account::{AccountModelAccess, AccountModelAccessMode};
use gateway_core::routing::{AccountGroupId, RoutingGroupSnapshot};

#[test]
fn shared_scope_intersects_groups_models_and_request_restrictions() {
    let group = AccountGroupId::new("grp_11111111111111111111111111111111").unwrap();
    let provider = ProviderKind::new("openai").unwrap();
    let direct = ProviderAccountId::new("acct_direct").unwrap();
    let forwarded = ProviderAccountId::new("acct_forwarded").unwrap();
    let unshared = ProviderAccountId::new("acct_unshared").unwrap();
    let directory = Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([
        (
            direct.clone(),
            RuntimeAccount::new(provider.clone(), BTreeSet::from([group.clone()]))
                .with_direct_authentication_kind(Some("oauth".into()))
                .with_model_access(
                    AccountModelAccess::new(
                        AccountModelAccessMode::Allowlist,
                        vec!["gpt-5".into()],
                    )
                    .unwrap(),
                ),
        ),
        (
            forwarded.clone(),
            RuntimeAccount::new(provider.clone(), BTreeSet::from([group.clone()])),
        ),
        (
            unshared.clone(),
            RuntimeAccount::new(provider.clone(), BTreeSet::new())
                .with_direct_authentication_kind(Some("oauth".into())),
        ),
    ])));
    let scope = FrozenAccountScope::new(
        directory.clone(),
        ClientRoutingScope::restricted(
            vec![RoutingGroupSnapshot::new(group.clone(), "共享组".into())],
            BTreeSet::from([group]),
            BTreeSet::from([provider]),
        )
        .unwrap(),
    )
    .requiring_groups();
    assert!(scope.allows_model(&direct, "gpt-5"));
    assert!(!scope.allows_model(&direct, "unshared-model"));
    assert!(scope.allows(&forwarded));
    assert!(!scope.allows(&unshared));
    let restricted = scope.restricted_to(&BTreeSet::new(), &BTreeSet::from([forwarded]));
    assert!(!restricted.provider_kinds().is_empty());
    assert!(!restricted.allows(&direct));
    assert!(!scope.excluding_account(direct.clone()).allows(&direct));
    assert!(
        !FrozenAccountScope::new(directory, ClientRoutingScope::AllAccounts)
            .requiring_groups()
            .allows(&direct)
    );
}
