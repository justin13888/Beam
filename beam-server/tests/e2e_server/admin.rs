//! Fixing a title's match and locking its fields (#185), through the admin
//! API over a scanned library.
//!
//! No provider is ever reached. TMDB is configured with a token that is not
//! one, and every HTTPS request the server makes goes to a proxy nothing
//! listens on: a fixed match is accepted and pinned exactly as it would be,
//! and the fetch it queues fails at once instead of reaching the internet.

use beam_test_support::e2e::StackOptions;
use beam_test_support::e2e::client::api;
use uuid::Uuid;

use crate::common::{Admin, VIEWER, Video, assert_refused};

/// A stack with TMDB configured and unreachable.
fn tmdb_unreachable() -> StackOptions {
    StackOptions::new()
        .env("BEAM_TMDB_API_TOKEN", "e2e-not-a-token")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("https_proxy", "http://127.0.0.1:9")
}

async fn enrichment(admin: &Admin, id: Uuid) -> api::types::MediaEnrichment {
    admin
        .api
        .get_media_enrichment(id)
        .await
        .unwrap_or_else(|e| panic!("read the enrichment of {id}: {e:?}"))
        .into_inner()
}

fn locks(fields: &[api::types::MetadataField]) -> api::types::SetFieldLocksRequest {
    api::types::SetFieldLocksRequest {
        locked_fields: fields.to_vec(),
    }
}

fn fix(external_ref: &str) -> api::types::FixMatchRequest {
    api::types::FixMatchRequest {
        external_ref: external_ref.to_owned(),
    }
}

/// An administrator fixes a title's match to a provider id: the title is
/// pinned to it as the administrator's pin, which outranks an NFO's; an id
/// another title holds, an id of an unconfigured provider and a malformed id
/// are refused; and clearing the fix hands the title back to its NFO's pin.
#[tokio::test]
async fn an_administrator_fixes_a_match_over_an_nfo_and_clears_it() {
    let mut admin = Admin::start_with(tmdb_unreachable()).await;
    admin
        .stack
        .library
        .add_video("films/Arrival (2016)/Arrival (2016).mp4", &Video::new(440));
    admin
        .stack
        .library
        .add_video("films/Heat (1995)/Heat (1995).mp4", &Video::new(550));
    admin.stack.library.write(
        "films/Heat (1995)/Heat (1995).nfo",
        br#"<movie><uniqueid type="tmdb">949</uniqueid></movie>"#,
    );
    let library = admin.add_library("Films", "films").await;
    admin.scan_ok(library).await;
    let arrival = admin.title("Arrival").await.id;
    let heat = admin.title("Heat").await.id;

    let pinned = enrichment(&admin, heat).await;
    assert_eq!(pinned.pinned_ref.as_deref(), Some("tmdb:949"));
    assert_eq!(
        pinned.pin_source,
        Some(api::types::MediaEnrichmentpinSource::Nfo)
    );
    let unpinned = enrichment(&admin, arrival).await;
    assert_eq!((unpinned.pinned_ref, unpinned.pin_source), (None, None));

    assert_refused(
        "a match to a provider that is not configured",
        409,
        admin.api.fix_media_match(arrival, &fix("anilist:21")).await,
    );
    assert_refused(
        "a match to an id another title is pinned to",
        409,
        admin.api.fix_media_match(arrival, &fix("tmdb:949")).await,
    );
    assert_refused(
        "a match to no provider's id",
        422,
        admin.api.fix_media_match(arrival, &fix("nope")).await,
    );
    assert_refused(
        "a match of no title",
        404,
        admin
            .api
            .fix_media_match(Uuid::new_v4(), &fix("tmdb:329865"))
            .await,
    );
    let untouched = enrichment(&admin, arrival).await;
    assert_eq!(untouched.pinned_ref, None, "a refused fix pins nothing");

    // Over the NFO's pin: the administrator's outranks it.
    let fixed = admin
        .api
        .fix_media_match(heat, &fix("tmdb:1"))
        .await
        .expect("fix Heat's match");
    assert_eq!(fixed.status().as_u16(), 202, "the fetch is queued, not run");
    let fixed = fixed.into_inner();
    assert_eq!(fixed.media_id, heat);
    assert_eq!(fixed.pinned_ref.as_deref(), Some("tmdb:1"));
    assert_eq!(
        fixed.pin_source,
        Some(api::types::MediaEnrichmentpinSource::Admin)
    );
    assert_eq!(fixed.matched_ref, None, "the old match is cleared");

    // A rescan reads the NFO again, and the administrator's pin stands.
    admin.scan_ok(library).await;
    let after_rescan = enrichment(&admin, heat).await;
    assert_eq!(
        (after_rescan.pinned_ref.as_deref(), after_rescan.pin_source),
        (
            Some("tmdb:1"),
            Some(api::types::MediaEnrichmentpinSource::Admin)
        ),
        "no NFO replaces an administrator's pin"
    );

    admin
        .api
        .clear_media_match(heat)
        .await
        .expect("clear Heat's fixed match");
    let cleared = enrichment(&admin, heat).await;
    assert_eq!(
        (cleared.pinned_ref.as_deref(), cleared.pin_source),
        (
            Some("tmdb:949"),
            Some(api::types::MediaEnrichmentpinSource::Nfo)
        ),
        "clearing the fix goes back to the NFO's pin"
    );

    // Clearing the match of a title no administrator pinned changes nothing.
    admin
        .api
        .clear_media_match(arrival)
        .await
        .expect("clear a match nobody fixed");
    assert_eq!(enrichment(&admin, arrival).await.pinned_ref, None);

    admin.finish().await;
}

/// Field locks are exactly the set last put, a show refuses the fields it
/// does not have, and only an administrator sees or changes any of it.
#[tokio::test]
async fn field_locks_are_set_whole_and_only_by_an_administrator() {
    use api::types::MetadataField as Field;

    let mut admin = Admin::start().await;
    admin
        .stack
        .library
        .add_video("films/Arrival (2016)/Arrival (2016).mp4", &Video::new(440));
    admin.stack.library.add_video(
        "shows/Severance/Season 01/Severance - S01E01.mp4",
        &Video::new(660),
    );
    let films = admin.add_library("Films", "films").await;
    let shows = admin.add_library("Shows", "shows").await;
    admin.scan_ok(films).await;
    admin.scan_ok(shows).await;
    let arrival = admin.title("Arrival").await.id;
    let severance = admin.title("Severance").await;
    assert!(severance.is_show);

    let set = admin
        .api
        .set_media_field_locks(arrival, &locks(&[Field::Title, Field::Poster]))
        .await
        .expect("lock Arrival's title and poster")
        .into_inner();
    let mut locked = set.locked_fields.clone();
    locked.sort_by_key(|field| format!("{field:?}"));
    assert_eq!(locked, [Field::Poster, Field::Title]);
    let mut read = enrichment(&admin, arrival).await.locked_fields;
    read.sort_by_key(|field| format!("{field:?}"));
    assert_eq!(read, locked, "the locks are stored");

    // The set is replaced whole, not merged.
    let replaced = admin
        .api
        .set_media_field_locks(arrival, &locks(&[Field::Genres]))
        .await
        .expect("lock only Arrival's genres")
        .into_inner();
    assert_eq!(replaced.locked_fields, [Field::Genres]);
    let none = admin
        .api
        .set_media_field_locks(arrival, &locks(&[]))
        .await
        .expect("unlock everything")
        .into_inner();
    assert!(none.locked_fields.is_empty());

    assert_refused(
        "locking a show's runtime",
        422,
        admin
            .api
            .set_media_field_locks(severance.id, &locks(&[Field::Title, Field::Runtime]))
            .await,
    );
    assert!(
        enrichment(&admin, severance.id)
            .await
            .locked_fields
            .is_empty(),
        "a refused set locks nothing"
    );
    admin
        .api
        .set_media_field_locks(severance.id, &locks(&[Field::Title]))
        .await
        .expect("a show's title locks");

    let viewer = admin.stack.client(&admin.stack.sign_in(VIEWER).await);
    assert_refused(
        "a viewer reading a title's enrichment",
        403,
        viewer.get_media_enrichment(arrival).await,
    );
    assert_refused(
        "a viewer locking a field",
        403,
        viewer
            .set_media_field_locks(arrival, &locks(&[Field::Title]))
            .await,
    );
    assert_refused(
        "a viewer fixing a match",
        403,
        viewer.fix_media_match(arrival, &fix("tmdb:329865")).await,
    );
    assert_refused(
        "a viewer clearing a match",
        403,
        viewer.clear_media_match(arrival).await,
    );

    admin.finish().await;
}
