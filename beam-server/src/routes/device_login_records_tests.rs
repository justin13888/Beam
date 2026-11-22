//! What a device login leaves in the admin log (NFR-403).
//!
//! A child of `device_login_tests`, so these share its harness and drive the
//! same router, fakes and in-memory stores.

use super::*;

/// Every `Auth` entry the harness's admin log holds, oldest first.
pub(super) async fn sign_in_records(harness: &Harness) -> Vec<beam_domain::models::AdminLog> {
    let mut entries = harness
        .admin_log
        .list_by_category(beam_domain::models::AdminLogCategory::Auth, 100, 0)
        .await
        .unwrap();
    entries.reverse();
    entries
}

/// NFR-403: a refused and an approved device login are each recorded with
/// the method, the outcome and the account -- and neither the device handle,
/// the IdP's device code, nor the minted session token reaches the record.
/// A poll of a flow that does not exist is not recorded: any request can
/// make one.
#[tokio::test]
async fn a_denied_and_an_approved_device_login_are_recorded_without_secrets() {
    let denied =
        harness(FakeOidcClient::default().with_device_script(vec![Ok(DevicePoll::Denied)]));
    let denied_handle = started(&denied).await;
    poll(&denied, &denied_handle)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    poll(&denied, "never-issued")
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    let approved = harness(
        FakeOidcClient::default()
            .with_device_script(vec![Ok(DevicePoll::Complete(identity(json!({}))))]),
    );
    let handle = started(&approved).await;
    let response = poll(&approved, &handle).await;
    response.assert_status(StatusCode::OK);
    let body: Value = response.json();
    let token = body["session_token"].as_str().expect("a session token");

    let refusals = sign_in_records(&denied).await;
    assert_eq!(
        refusals.len(),
        1,
        "only the open flow's outcome: {refusals:?}"
    );
    let refusal = refusals[0].details.clone().unwrap_or_default();
    assert_eq!(
        (
            &refusals[0].level,
            refusal["method"].as_str(),
            refusal["outcome"].as_str()
        ),
        (
            &beam_domain::models::AdminLogLevel::Warning,
            Some("device"),
            Some("denied")
        )
    );

    let sign_ins = sign_in_records(&approved).await;
    assert_eq!(sign_ins.len(), 1, "{sign_ins:?}");
    let sign_in = sign_ins[0].details.clone().unwrap_or_default();
    assert_eq!(
        (
            &sign_ins[0].level,
            sign_in["method"].as_str(),
            sign_in["outcome"].as_str(),
            sign_in["user_id"].as_str()
        ),
        (
            &beam_domain::models::AdminLogLevel::Info,
            Some("device"),
            Some("signed_in"),
            body["user"]["id"].as_str()
        )
    );

    let stored = format!("{refusals:?} {sign_ins:?}");
    let mut secrets = vec![token.to_owned(), handle, denied_handle];
    secrets.extend(denied.oidc.polled_device_codes());
    secrets.extend(approved.oidc.polled_device_codes());
    for secret in secrets {
        assert!(!stored.contains(&secret), "{secret} reached the admin log");
    }
}

/// NFR-403: an open device login that expires -- because the identity
/// provider says so, or because it outlived its lifetime here -- is recorded
/// once, as an expired device sign-in, and its handle never reaches the row.
#[tokio::test]
async fn an_expired_device_login_is_recorded_as_expired() {
    let reported =
        harness(FakeOidcClient::default().with_device_script(vec![Ok(DevicePoll::Expired)]));
    let reported_handle = started(&reported).await;
    poll(&reported, &reported_handle)
        .await
        .assert_status(StatusCode::GONE);

    let outlived = harness(FakeOidcClient::default());
    let outlived_handle = started(&outlived).await;
    outlived
        .clock
        .advance(Duration::from_secs(FAKE_DEVICE_EXPIRES_IN_SECS));
    poll(&outlived, &outlived_handle)
        .await
        .assert_status(StatusCode::GONE);

    for (harness, handle) in [(&reported, reported_handle), (&outlived, outlived_handle)] {
        let records = sign_in_records(harness).await;
        let summary: Vec<(beam_domain::models::AdminLogLevel, Value)> = records
            .iter()
            .map(|entry| {
                let details = entry.details.clone().unwrap_or_default();
                (
                    entry.level.clone(),
                    json!([details["method"], details["outcome"]]),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![(
                beam_domain::models::AdminLogLevel::Warning,
                json!(["device", "expired"])
            )]
        );
        assert!(
            !format!("{records:?}").contains(&handle),
            "the device handle reached the admin log"
        );
    }
}
