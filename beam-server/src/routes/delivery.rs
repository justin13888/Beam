//! A ranged delivery whose `Content-Type` is chosen at request time.
//!
//! `Delivery<M>` carries its media type at the type level so it can describe
//! itself, which is right for an operation serving one representation and wrong
//! for a media server: Beam's content type comes from the bytes being served --
//! the container the indexer detected for a source file, the format a provider
//! returned for a poster. This wrapper keeps Kynos's range engine and replaces
//! only that header, then describes the media-type ranges the operation
//! actually answers with.
//!
//! Nothing here is an escape hatch. `IntoResponse` and `Responses` are the
//! public traits every response type implements, and `delivery_responses` is
//! public precisely so a wrapper can build on it -- the description stays
//! accurate rather than being waived, and the `unchecked` feature stays off.
//!
//! It is still a gap worth closing upstream: a ranged delivery whose media type
//! is chosen at request time is a normal thing for a file server to want, and
//! Kynos has no way to say it. Recorded in
//! `docs/architecture/kynos-migration-readiness.md`.
//!
//! Shared rather than per-route because the rule about *which* statuses may be
//! relabelled is subtle enough that a second copy would drift from this one:
//! two operations serve bytes whose type is only known at request time, and
//! both need the same answer.

use std::marker::PhantomData;

use kynos::extract::media::MediaType;
use kynos::response::range::served::Delivery;
use kynos::response::{IntoResponse, Responses};

/// The media type the range engine is parameterised by.
///
/// A placeholder: what is served is decided per request and cannot be a
/// `const`, so [`RuntimeDelivery`] overrides the header and describes the range
/// of types honestly.
pub struct AnyMedia;

impl MediaType for AnyMedia {
    const MEDIA_TYPE: &'static str = "application/octet-stream";
}

/// The media-type ranges one operation can answer with.
///
/// Type-level because `Responses::responses` describes an operation without an
/// instance of it to consult.
pub trait MediaRanges {
    /// The ranges this operation answers with, most representative first.
    /// Must not be empty.
    const RANGES: &'static [&'static str];
}

/// A ranged delivery labelled with the content type of the bytes it carries.
pub struct RuntimeDelivery<R: MediaRanges> {
    inner: Delivery<AnyMedia>,
    content_type: String,
    ranges: PhantomData<fn() -> R>,
}

impl<R: MediaRanges> RuntimeDelivery<R> {
    pub const fn new(inner: Delivery<AnyMedia>, content_type: String) -> Self {
        Self {
            inner,
            content_type,
            ranges: PhantomData,
        }
    }
}

impl<R: MediaRanges> IntoResponse for RuntimeDelivery<R> {
    fn into_response(self) -> kynos::http::Response {
        let Self {
            inner,
            content_type,
            ranges: _,
        } = self;

        let status = inner.status();
        let mut response = inner.into_response();

        // Only the two statuses that actually carry the bytes get their
        // content type. Everything else keeps whatever the range engine
        // labelled it.
        //
        // The check used to be `!= 304`, which was right about 304 -- a
        // response with no representation carries no content type, RFC 9110
        // section 15.4.5 -- and wrong about everything else the engine can
        // produce. A `Range` that cannot be satisfied answers 416 with an
        // RFC 9457 problem document, and that was being relabelled
        // `video/mp4`: a JSON body announced as video, which a client is
        // entitled to fail on. Naming the two statuses that mean "here are the
        // bytes" states the rule positively, so a status added later is
        // excluded by default rather than mislabelled by default.
        if matches!(
            status,
            kynos::http::StatusCode::OK | kynos::http::StatusCode::PARTIAL_CONTENT
        ) && let Ok(value) = content_type.parse()
        {
            response
                .headers_mut()
                .insert(kynos::http::header::CONTENT_TYPE, value);
        }

        response
    }
}

impl<R: MediaRanges> Responses for RuntimeDelivery<R> {
    /// The same 200/206/304 shape `Delivery` describes, widened to every range
    /// in `R`.
    ///
    /// OpenAPI permits a media-type range (`video/*`, `image/*`) as a `content`
    /// key, which is the honest description of "whatever was detected".
    fn responses(registry: &mut kynos::schema::registry::Registry) -> kynos::openapi::Responses {
        use kynos::openapi::{MediaType as OpenApiMediaType, RefOr, Schema, StatusPattern};

        let _ = registry;
        let (primary, rest) = R::RANGES
            .split_first()
            .expect("a delivery answers with at least one media type");
        let mut responses = kynos::response::range::delivery_responses(primary);

        for status in [200, 206] {
            if let Some(RefOr::Item(response)) = responses
                .responses
                .get_mut(&StatusPattern::Code(status).to_string())
            {
                for media_type in rest {
                    response.content.insert(
                        (*media_type).to_owned(),
                        OpenApiMediaType::new(Schema::Object(Box::default())),
                    );
                }
            }
        }

        responses
    }
}
