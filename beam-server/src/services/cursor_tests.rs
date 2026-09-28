use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use beam_domain::models::catalog::{CatalogPosition, SortKey, TitleKind};
use chrono::{DateTime, Utc};
use proptest::prelude::*;
use uuid::Uuid;

use super::{CursorError, decode, encode};
use crate::models::search::{MediaSortField, SortOrder};

const FIELDS: [MediaSortField; 5] = [
    MediaSortField::Title,
    MediaSortField::Year,
    MediaSortField::Rating,
    MediaSortField::DateAdded,
    MediaSortField::Runtime,
];

fn any_field() -> impl Strategy<Value = MediaSortField> {
    proptest::sample::select(FIELDS.to_vec())
}

fn any_order() -> impl Strategy<Value = SortOrder> {
    prop_oneof![Just(SortOrder::Asc), Just(SortOrder::Desc)]
}

/// A key of `field`'s kind, over the whole range the store can hold.
fn any_key(field: MediaSortField) -> BoxedStrategy<SortKey> {
    match field {
        MediaSortField::Title => ".*".prop_map(SortKey::Title).boxed(),
        MediaSortField::Year => proptest::option::of(any::<i32>())
            .prop_map(SortKey::Year)
            .boxed(),
        MediaSortField::Runtime => proptest::option::of(any::<i32>())
            .prop_map(SortKey::Runtime)
            .boxed(),
        MediaSortField::Rating => proptest::option::of(
            any::<f32>().prop_filter("a stored rating is a number", |r| r.is_finite()),
        )
        .prop_map(SortKey::Rating)
        .boxed(),
        MediaSortField::DateAdded => (0_i64..4_102_444_800, 0_u32..1_000_000_000)
            .prop_map(|(secs, nanos)| {
                SortKey::DateAdded(DateTime::<Utc>::from_timestamp(secs, nanos).expect("in range"))
            })
            .boxed(),
    }
}

fn any_position_for(field: MediaSortField) -> impl Strategy<Value = CatalogPosition> {
    (
        prop_oneof![Just(TitleKind::Movie), Just(TitleKind::Show)],
        any::<u128>(),
        any_key(field),
    )
        .prop_map(|(kind, id, key)| CatalogPosition {
            kind,
            id: Uuid::from_u128(id),
            key,
        })
}

fn raw(json: &str) -> String {
    URL_SAFE_NO_PAD.encode(json)
}

proptest! {
    /// What `encode` writes, `decode` reads back exactly -- a lossy key would
    /// seek a page from the wrong place.
    #[test]
    fn a_cursor_round_trips_its_position(
        (field, order, position) in (any_field(), any_order())
            .prop_flat_map(|(field, order)| (Just(field), Just(order), any_position_for(field)))
    ) {
        let cursor = encode(field, order, &position);
        prop_assert_eq!(decode(&cursor, field, order), Ok(position));
    }

    /// A cursor pages only the sort it was issued for: under any other it
    /// names a meaningless place.
    #[test]
    fn a_cursor_is_refused_under_any_other_sort(
        (field, order, position, other_field, other_order) in (any_field(), any_order())
            .prop_flat_map(|(field, order)| {
                (Just(field), Just(order), any_position_for(field), any_field(), any_order())
            })
    ) {
        prop_assume!((field, order) != (other_field, other_order));
        let cursor = encode(field, order, &position);
        prop_assert!(
            matches!(decode(&cursor, other_field, other_order), Err(CursorError::SortMismatch { .. })),
            "{:?}", decode(&cursor, other_field, other_order)
        );
    }

    /// Whatever a client sends, decoding answers rather than panics.
    #[test]
    fn decoding_any_text_never_panics(text in ".*", field in any_field(), order in any_order()) {
        let _ = decode(&text, field, order);
        let _ = decode(&raw(&text), field, order);
    }
}

#[test]
fn a_cursor_the_server_did_not_write_is_malformed() {
    let id = Uuid::from_u128(1);
    for (cursor, field) in [
        ("not base64 at all!".to_string(), MediaSortField::Title),
        (raw("not json"), MediaSortField::Title),
        (
            raw(&format!(
                r#"{{"v":1,"sort":"title","order":"asc","kind":"movie","id":"{id}"}}"#
            )),
            MediaSortField::Title,
        ),
        (
            raw(&format!(
                r#"{{"v":1,"sort":"title","order":"asc","kind":"episode","id":"{id}","key":"a"}}"#
            )),
            MediaSortField::Title,
        ),
        (
            raw(&format!(
                r#"{{"v":1,"sort":"year","order":"asc","kind":"movie","id":"{id}","key":"1999"}}"#
            )),
            MediaSortField::Year,
        ),
        (
            raw(&format!(
                r#"{{"v":1,"sort":"rating","order":"asc","kind":"movie","id":"{id}","key":0.1}}"#
            )),
            MediaSortField::Rating,
        ),
        (
            raw(&format!(
                r#"{{"v":1,"sort":"date_added","order":"asc","kind":"movie","id":"{id}","key":"yesterday"}}"#
            )),
            MediaSortField::DateAdded,
        ),
        (
            raw(&format!(
                r#"{{"v":1,"sort":"title","order":"asc","kind":"movie","id":"{id}","key":"a","extra":1}}"#
            )),
            MediaSortField::Title,
        ),
    ] {
        assert_eq!(
            decode(&cursor, field, SortOrder::Asc),
            Err(CursorError::Malformed),
            "{cursor}"
        );
    }
}

#[test]
fn a_cursor_from_another_layout_version_is_refused_as_such() {
    let cursor = raw(&format!(
        r#"{{"v":2,"sort":"title","order":"asc","kind":"movie","id":"{}","key":"a"}}"#,
        Uuid::from_u128(1)
    ));
    assert_eq!(
        decode(&cursor, MediaSortField::Title, SortOrder::Asc),
        Err(CursorError::WrongVersion)
    );
}

/// A missing year is a key of its own -- it seeks among the titles without
/// one -- not the absence of a key.
#[test]
fn a_missing_value_is_a_key_not_a_missing_key() {
    let position = CatalogPosition {
        kind: TitleKind::Show,
        id: Uuid::from_u128(9),
        key: SortKey::Year(None),
    };
    let cursor = encode(MediaSortField::Year, SortOrder::Desc, &position);
    assert_eq!(
        decode(&cursor, MediaSortField::Year, SortOrder::Desc),
        Ok(position)
    );
}
