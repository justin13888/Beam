//! Media grant signing and verification (ADR-0022), over fixed instants.
//!
//! The routes that issue and accept grants are tested end to end in
//! `routes/stream_tests.rs` and `routes/access_tests.rs`; this pins the
//! properties those cannot reach one by one: which bytes the signature covers,
//! and where exactly the expiry falls.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use proptest::prelude::*;
use uuid::Uuid;

use super::{GrantClaims, GrantRefused, MEDIA_GRANT_TTL_SECS, MediaGrantSigner};

const FILE: Uuid = Uuid::from_u128(0x1111_1111_1111_4111_8111_1111_1111_1111);
const USER: Uuid = Uuid::from_u128(0x2222_2222_2222_4222_8222_2222_2222_2222);

fn issued_at() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-03-01T20:15:30.750Z")
        .expect("a fixed instant")
        .with_timezone(&Utc)
}

fn signer() -> MediaGrantSigner {
    MediaGrantSigner::with_key(b"a key only these tests use")
}

#[test]
fn a_grant_verifies_to_the_file_and_user_it_was_issued_for() {
    let signer = signer();
    let issued = signer.issue(FILE, USER, issued_at());

    let expected_expiry =
        DateTime::from_timestamp(issued_at().timestamp() + MEDIA_GRANT_TTL_SECS, 0)
            .expect("in range");
    assert_eq!(issued.expires_at, expected_expiry);
    assert_eq!(
        signer.verify(&issued.token, issued_at()),
        Ok(GrantClaims {
            file_id: FILE,
            user_id: USER,
            expires_at: expected_expiry,
        })
    );
}

#[test]
fn a_grant_is_honoured_until_its_expiry_and_refused_from_it() {
    let signer = signer();
    let issued = signer.issue(FILE, USER, issued_at());

    assert!(
        signer
            .verify(&issued.token, issued.expires_at - Duration::seconds(1))
            .is_ok(),
        "a second before the expiry"
    );
    assert_eq!(
        signer.verify(&issued.token, issued.expires_at),
        Err(GrantRefused::Expired)
    );
}

#[test]
fn a_grant_from_another_key_does_not_verify() {
    let issued = MediaGrantSigner::with_key(b"someone else's key").issue(FILE, USER, issued_at());

    assert_eq!(
        signer().verify(&issued.token, issued_at()),
        Err(GrantRefused::BadSignature)
    );
}

#[test]
fn two_random_signers_do_not_share_a_key() {
    let issued = MediaGrantSigner::random().issue(FILE, USER, issued_at());

    assert_eq!(
        MediaGrantSigner::random().verify(&issued.token, issued_at()),
        Err(GrantRefused::BadSignature),
        "a restart voids every grant outstanding"
    );
}

#[test]
fn what_is_not_a_grant_is_malformed() {
    let signer = signer();
    let issued = signer.issue(FILE, USER, issued_at());

    for token in [
        "",
        "not base64url!",
        &issued.token[..issued.token.len() - 4],
        &format!("{}AAAA", issued.token),
    ] {
        assert_eq!(
            signer.verify(token, issued_at()),
            Err(GrantRefused::Malformed),
            "{token:?}"
        );
    }
}

#[test]
fn the_signers_debug_never_prints_its_key() {
    let printed = format!("{:?}", signer());

    assert!(!printed.contains("a key only"), "{printed}");
}

proptest! {
    /// Every bit of the token is covered: changing any one -- the file, the
    /// user, the expiry or the tag itself -- never yields a grant.
    #[test]
    fn no_single_bit_of_a_grant_can_be_changed(bit in 0usize..(72 * 8)) {
        let signer = signer();
        let issued = signer.issue(FILE, USER, issued_at());
        let mut bytes = URL_SAFE_NO_PAD.decode(&issued.token).expect("a grant is base64url");
        bytes[bit / 8] ^= 1 << (bit % 8);

        prop_assert_eq!(
            signer.verify(&URL_SAFE_NO_PAD.encode(&bytes), issued_at()),
            Err(GrantRefused::BadSignature)
        );
    }
}
