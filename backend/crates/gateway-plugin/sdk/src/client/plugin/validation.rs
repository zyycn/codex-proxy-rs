use std::collections::BTreeMap;

use crate::{Capability as C, Manifest};

use super::{AuthorError, Handler, methods};

pub(super) fn validate(
    manifest: &Manifest,
    handlers: &BTreeMap<&'static str, Handler>,
) -> Result<(), AuthorError> {
    let required = |method| {
        handlers
            .contains_key(method)
            .then_some(())
            .ok_or(AuthorError::MissingMethod(method))
    };
    for capability in manifest.contributes.keys() {
        match capability {
            C::Maintenance => required(methods::RECONCILE.name)?,
            C::Middleware => required(crate::call::middleware::HANDLE_METHOD)?,
            C::ModelRouter => required(methods::ROUTE_MODEL.name)?,
            C::ModelCatalog => required(methods::MODEL_CATALOG_REGISTER.name)?,
            C::RetryPolicy => required(methods::RETRY_DECISION.name)?,
            C::Scheduler => required(methods::SCHEDULE_ACCOUNT.name)?,
            C::RequestLifecycle | C::Usage => required(methods::OBSERVE_REQUEST.name)?,
            C::WebSocketObserver => required(methods::OBSERVE_WEBSOCKET.name)?,
            C::Management => {
                required(methods::MANAGEMENT_REGISTER.name)?;
                required(methods::MANAGEMENT_HANDLE.name)?;
            }
            C::CommandLine => {
                required(methods::COMMAND_LINE_REGISTER.name)?;
                required(methods::COMMAND_LINE_EXECUTE.name)?;
            }
            C::FrontendAuthentication => {
                required(methods::FRONTEND_IDENTIFIER.name)?;
                required(methods::FRONTEND_AUTHENTICATE.name)?;
            }
        }
    }
    if manifest
        .state
        .iter()
        .any(|namespace| !namespace.migrates_from.is_empty())
    {
        required(methods::STATE_MIGRATE.name)?;
    }

    Ok(())
}
