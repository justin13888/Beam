//! The caller's access policy (issue #263), as the routes reading or
//! delivering a title apply it: who the session's user is, what their policy
//! hides, and the 404 a hidden id answers with.

use beam_domain::models::access::AccessPolicy;
use uuid::Uuid;

use crate::routes::api_error::{InternalError, ServerFault, SessionAuth};
use crate::state::AppState;

/// Resolves the session's user id.
///
/// Kept fallible rather than defaulting: every watched-state row is keyed by
/// this value, so resolving a malformed id to the nil UUID would pool every
/// affected user's history and progress into one shared, unowned account.
pub(crate) fn parse_user_id(user_id: &str) -> Result<Uuid, InternalError> {
    Uuid::parse_str(user_id)
        .map_err(|_| InternalError::Internal(ServerFault::new("invalid user id in session")))
}

/// The session's user id and their access policy (issue #263): what every
/// route reading or delivering a title checks the ids it is given against.
pub(crate) async fn caller_access(
    state: &AppState,
    auth: &SessionAuth,
) -> Result<(Uuid, AccessPolicy), InternalError> {
    let user_id = parse_user_id(&auth.0.user_id)?;
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

#[cfg(test)]
mod parse_user_id_tests {
    use super::*;

    #[test]
    fn a_well_formed_session_user_id_parses_to_itself() {
        let id = Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0);
        assert_eq!(parse_user_id(&id.to_string()).unwrap(), id);
    }

    #[test]
    fn a_malformed_user_id_is_an_error_not_the_nil_uuid() {
        // Every watched-state row is keyed by this value. Defaulting to the nil
        // UUID on a parse failure would pool every affected user's history
        // and progress into one shared, unowned account.
        for malformed in [
            "",
            "not-a-uuid",
            "1234",
            "00000000-0000-0000-0000-00000000000",
        ] {
            let error = parse_user_id(malformed)
                .expect_err("a malformed session user id must not resolve to an account");
            let InternalError::Internal(_) = error;
        }
    }

    #[test]
    fn the_nil_uuid_is_only_produced_when_it_was_actually_asked_for() {
        assert_eq!(
            parse_user_id(&Uuid::nil().to_string()).unwrap(),
            Uuid::nil()
        );
    }
}
