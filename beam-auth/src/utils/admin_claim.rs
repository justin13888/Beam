//! Admin status is derived solely from a claim the IdP asserts in the verified
//! ID token, recomputed on every OIDC login rather than trusted from a stored
//! flag (see ADR-0003 and issue #85). Trusting the IdP as the single authority
//! -- instead of a server-side `BEAM_ADMIN_EMAILS` allowlist -- keeps the
//! admin attack surface minimal to audit: there is no side-channel grant to
//! reconcile, and revoking admin is just removing the claim (e.g. a group) at
//! the IdP and waiting for the next login.

use serde_json::Value;

/// Evaluates whether the verified ID-token `claims` grant admin, given the
/// deployment-configured `claim_name` and optional `expected` value.
///
/// Matching semantics (case-sensitive on values):
/// * `expected == None` -- the claim must assert boolean `true`. A stringified
///   `"true"` is also accepted, for IdPs that render booleans as strings.
/// * `expected == Some(value)` -- the claim matches when it is a string equal
///   to `value`, or an array containing that string (covers a `groups` claim
///   like `["beam-admin", ...]`).
///
/// A claim that is absent, `null`, or of an unexpected JSON shape never grants
/// admin. `claims` that is not a JSON object grants admin to nobody.
pub fn admin_claim_matches(claims: &Value, claim_name: &str, expected: Option<&str>) -> bool {
    let Some(value) = claims.get(claim_name) else {
        return false;
    };

    match expected {
        None => match value {
            Value::Bool(b) => *b,
            Value::String(s) => s == "true",
            _ => false,
        },
        Some(expected) => match value {
            Value::String(s) => s == expected,
            Value::Array(items) => items.iter().any(|item| item.as_str() == Some(expected)),
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ─── expected == None: boolean-claim semantics ──────────────────────────

    #[test]
    fn boolean_true_claim_grants_admin() {
        let claims = json!({ "is_admin": true });
        assert!(admin_claim_matches(&claims, "is_admin", None));
    }

    #[test]
    fn boolean_false_claim_denies_admin() {
        let claims = json!({ "is_admin": false });
        assert!(!admin_claim_matches(&claims, "is_admin", None));
    }

    #[test]
    fn stringified_true_claim_grants_admin() {
        // Some IdPs render every claim as a string.
        let claims = json!({ "is_admin": "true" });
        assert!(admin_claim_matches(&claims, "is_admin", None));
    }

    #[test]
    fn other_string_denies_admin_when_no_value_expected() {
        let claims = json!({ "is_admin": "false" });
        assert!(!admin_claim_matches(&claims, "is_admin", None));
        let claims = json!({ "is_admin": "yes" });
        assert!(!admin_claim_matches(&claims, "is_admin", None));
    }

    #[test]
    fn non_boolean_shapes_deny_admin_when_no_value_expected() {
        // A number, array, or object where a bool was expected never grants.
        assert!(!admin_claim_matches(
            &json!({ "is_admin": 1 }),
            "is_admin",
            None
        ));
        assert!(!admin_claim_matches(
            &json!({ "is_admin": ["true"] }),
            "is_admin",
            None
        ));
    }

    // ─── expected == Some(value): string / array-contains semantics ─────────

    #[test]
    fn string_claim_equal_to_expected_grants_admin() {
        let claims = json!({ "role": "beam-admin" });
        assert!(admin_claim_matches(&claims, "role", Some("beam-admin")));
    }

    #[test]
    fn string_claim_not_equal_to_expected_denies_admin() {
        let claims = json!({ "role": "viewer" });
        assert!(!admin_claim_matches(&claims, "role", Some("beam-admin")));
    }

    #[test]
    fn matching_is_case_sensitive() {
        let claims = json!({ "role": "Beam-Admin" });
        assert!(!admin_claim_matches(&claims, "role", Some("beam-admin")));
    }

    #[test]
    fn array_claim_containing_expected_grants_admin() {
        let claims = json!({ "groups": ["users", "beam-admin", "staff"] });
        assert!(admin_claim_matches(&claims, "groups", Some("beam-admin")));
    }

    #[test]
    fn array_claim_not_containing_expected_denies_admin() {
        let claims = json!({ "groups": ["users", "staff"] });
        assert!(!admin_claim_matches(&claims, "groups", Some("beam-admin")));
    }

    #[test]
    fn array_with_non_string_elements_is_handled() {
        // Mixed/garbage array elements must not panic or false-match.
        let claims = json!({ "groups": [1, true, "beam-admin"] });
        assert!(admin_claim_matches(&claims, "groups", Some("beam-admin")));
        let claims = json!({ "groups": [1, true, null] });
        assert!(!admin_claim_matches(&claims, "groups", Some("beam-admin")));
    }

    #[test]
    fn boolean_claim_denies_admin_when_a_value_is_expected() {
        // With ADMIN_VALUE set the claim must be a matching string/array, not a bool.
        let claims = json!({ "role": true });
        assert!(!admin_claim_matches(&claims, "role", Some("beam-admin")));
    }

    // ─── missing / malformed inputs ─────────────────────────────────────────

    #[test]
    fn missing_claim_denies_admin_in_both_modes() {
        let claims = json!({ "sub": "abc" });
        assert!(!admin_claim_matches(&claims, "groups", None));
        assert!(!admin_claim_matches(&claims, "groups", Some("beam-admin")));
    }

    #[test]
    fn null_claim_denies_admin() {
        let claims = json!({ "groups": null });
        assert!(!admin_claim_matches(&claims, "groups", None));
        assert!(!admin_claim_matches(&claims, "groups", Some("beam-admin")));
    }

    #[test]
    fn non_object_claims_deny_admin() {
        // A non-object claim set (should never happen for a real ID token, but
        // must be handled) grants nobody.
        assert!(!admin_claim_matches(&Value::Null, "groups", None));
        assert!(!admin_claim_matches(
            &json!("not-an-object"),
            "groups",
            None
        ));
        assert!(!admin_claim_matches(&json!(["a"]), "groups", Some("a")));
    }
}
