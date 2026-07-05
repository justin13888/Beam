//! Admin status is recomputed from an email allowlist on every OIDC login
//! rather than trusted from a stored flag (see ADR-0003) -- revoking admin
//! access is then just editing `BEAM_ADMIN_EMAILS` and waiting for the next
//! login, no database write required.

/// Checks `email` against a comma-separated allowlist, case-insensitively.
/// Surrounding whitespace around each entry is ignored. An empty or absent
/// allowlist matches nothing.
pub fn is_admin_email(email: &str, allowlist_csv: &str) -> bool {
    let email = email.trim().to_ascii_lowercase();
    if email.is_empty() {
        return false;
    }
    allowlist_csv
        .split(',')
        .map(|entry| entry.trim().to_ascii_lowercase())
        .any(|entry| !entry.is_empty() && entry == email)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_exact_email_in_list() {
        assert!(is_admin_email(
            "admin@beam.localhost",
            "admin@beam.localhost,other@example.com"
        ));
    }

    #[test]
    fn matches_case_insensitively() {
        assert!(is_admin_email(
            "Admin@Beam.Localhost",
            "admin@beam.localhost"
        ));
    }

    #[test]
    fn ignores_surrounding_whitespace_in_list_entries() {
        assert!(is_admin_email(
            "admin@beam.localhost",
            " admin@beam.localhost , other@example.com "
        ));
    }

    #[test]
    fn rejects_email_not_in_list() {
        assert!(!is_admin_email("user@example.com", "admin@beam.localhost"));
    }

    #[test]
    fn empty_allowlist_matches_nothing() {
        assert!(!is_admin_email("admin@beam.localhost", ""));
    }

    #[test]
    fn empty_email_never_matches() {
        assert!(!is_admin_email("", "admin@beam.localhost"));
    }

    #[test]
    fn does_not_substring_match() {
        // "admin@beam.localhost" must not match as a substring of a longer entry.
        assert!(!is_admin_email(
            "admin@beam.localhost",
            "notadmin@beam.localhost"
        ));
    }
}
