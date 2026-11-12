//! The caller's access policy (issue #263), as the routes reading or
//! delivering a title apply it: who the session's user is, what their policy
//! hides, and the 404 a hidden id answers with.

use beam_domain::models::access::AccessPolicy;
use uuid::Uuid;

use crate::routes::api_error::{InternalError, ServerFault};
use crate::routes::session_auth::SessionAuth;
use crate::state::AppState;

/// The session's user id and their access policy (issue #263): what every
/// route reading or delivering a title checks the ids it is given against.
pub(crate) async fn caller_access(
    state: &AppState,
    auth: &SessionAuth,
) -> Result<(Uuid, AccessPolicy), InternalError> {
    let user_id = auth.0.user_id;
    let policy = state
        .services
        .access
        .policy(user_id)
        .await
        .map_err(|err| InternalError::Internal(ServerFault::new(err)))?;
    Ok((user_id, policy))
}

/// Whether `policy` hides what `id` names, a failed read as a 500.
pub(crate) async fn hides(
    state: &AppState,
    policy: &AccessPolicy,
    id: Uuid,
) -> Result<bool, InternalError> {
    state
        .services
        .access
        .hides(policy, id)
        .await
        .map_err(|err| InternalError::Internal(ServerFault::new(err)))
}

/// The detail of the 404 a hidden id answers with: the one an id naming
/// nothing gets, so the two cannot be told apart.
pub(crate) fn media_not_found(id: Uuid) -> String {
    format!("media {id} not found")
}
