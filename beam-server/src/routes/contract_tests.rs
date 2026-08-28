//! Contract tests binding the router to the things that are supposed to
//! describe it: the generated OpenAPI document and the metrics route
//! classifier.
//!
//! Both of those are hand-maintained mirrors of the route table, and a mirror
//! drifts. Writing a second copy of the table into a test does not help -- the
//! copy drifts in lockstep with the thing it copies, which is why the previous
//! `classify_route` test could not fail when a route was added. These tests
//! derive the route table from `create_docs_router` itself, so adding a route
//! and forgetting either mirror is a failure rather than a silent gap.

use std::collections::BTreeSet;

use salvo::prelude::*;

use crate::routes::{create_docs_router, metrics_mw::classify_route};

/// One route as the router itself reports it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Route {
    method: String,
    /// Full path, leading slash, `{param}` placeholders intact.
    path: String,
}

/// Every route registered in `create_docs_router`, read out of the router's
/// own tree rather than restated here.
///
/// Salvo does not expose its route table programmatically, but it renders the
/// whole tree through `Debug` -- path segments as nodes, `[METHOD] -> handler`
/// as leaves. Parsing that is indirect, but it is the *router's* answer; a
/// hand-written list would be a second source of truth, which is the failure
/// this test exists to prevent. If a future salvo changes the rendering, this
/// parser fails loudly (it asserts it found routes) rather than silently
/// passing on an empty set.
fn router_routes() -> BTreeSet<Route> {
    let rendered = format!("{:?}", create_docs_router());
    let mut stack: Vec<String> = Vec::new();
    let mut routes = BTreeSet::new();

    for line in rendered.lines() {
        let Some(marker) = line.rfind("──") else {
            continue;
        };
        // Each nesting level is four columns of box-drawing prefix.
        let depth = line[..marker].chars().count() / 4;
        let label = line[marker + "──".len()..].trim();

        if let Some(rest) = label.strip_prefix('[') {
            // A leaf: `[GET] -> handler::path`.
            let Some((method, _handler)) = rest.split_once(']') else {
                continue;
            };
            let path = stack
                .iter()
                .filter(|segment| !segment.is_empty())
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("/");
            routes.insert(Route {
                method: method.to_string(),
                path: format!("/{path}"),
            });
            continue;
        }

        // A path segment. `!NULL!` is salvo's rendering of a router with no
        // path of its own (a pure grouping node).
        let segment = if label == "!NULL!" { "" } else { label };
        stack.truncate(depth);
        stack.push(segment.to_string());
    }

    assert!(
        !routes.is_empty(),
        "failed to read any route out of the router tree; the parser above no \
         longer matches salvo's Debug rendering:\n{rendered}"
    );
    routes
}

/// Every `(method, path)` in the OpenAPI document generated from the same
/// router.
fn documented_routes() -> BTreeSet<Route> {
    let doc = OpenApi::new("Beam Server API", "1.0.0").merge_router(&create_docs_router());
    let mut routes = BTreeSet::new();
    for (path, item) in doc.paths.iter() {
        for (method, _operation) in item.operations.iter() {
            routes.insert(Route {
                method: format!("{method:?}").to_uppercase(),
                path: path.clone(),
            });
        }
    }
    routes
}

#[test]
fn every_registered_route_is_documented_exactly_once() {
    let registered = router_routes();
    let documented = documented_routes();

    let undocumented: Vec<_> = registered.difference(&documented).collect();
    assert!(
        undocumented.is_empty(),
        "these routes are mounted but missing from the OpenAPI document, so the \
         generated client cannot call them -- annotate the handler with \
         `#[endpoint]`: {undocumented:#?}"
    );

    let phantom: Vec<_> = documented.difference(&registered).collect();
    assert!(
        phantom.is_empty(),
        "these operations are in the OpenAPI document but not mounted on the \
         router, so a generated client would call a 404: {phantom:#?}"
    );
}

#[test]
fn every_registered_route_has_a_metrics_class() {
    let unclassified: Vec<_> = router_routes()
        .into_iter()
        .filter(|route| {
            // The classifier reads a concrete request path, so substitute a
            // value for each `{param}` placeholder.
            let concrete = concrete_path(&route.path);
            let method = route.method.parse().unwrap_or(salvo::http::Method::GET);
            classify_route(&method, &concrete) == "other"
        })
        .collect();

    assert!(
        unclassified.is_empty(),
        "these routes fall through to the unbounded `other` metrics class -- \
         add them to `classify_route`: {unclassified:#?}"
    );
}

/// Replace every `{param}` placeholder with a concrete segment.
fn concrete_path(template: &str) -> String {
    template
        .split('/')
        .map(|segment| {
            if segment.starts_with('{') {
                "11111111-1111-1111-1111-111111111111"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[test]
fn routes_from_different_families_never_share_a_metrics_class() {
    // `classify_route` deliberately collapses *within* a family
    // (`/v1/libraries`, `/v1/libraries/{id}`, `/v1/libraries/{id}/files` are
    // all `libraries`). Collapsing *across* families would make the label
    // useless -- and is exactly what a copy-paste slip in the match arms
    // produces. The families are read from the router, so this cannot drift.
    use std::collections::BTreeMap;

    let mut class_to_families: BTreeMap<&'static str, BTreeSet<String>> = BTreeMap::new();
    for route in router_routes() {
        let family = route
            .path
            .trim_start_matches('/')
            .split('/')
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let method = route.method.parse().unwrap_or(salvo::http::Method::GET);
        class_to_families
            .entry(classify_route(&method, &concrete_path(&route.path)))
            .or_default()
            .insert(family);
    }

    // `session` is the one class that intentionally spans several top-level
    // paths -- `/v1/me`, `/v1/logout`, `/v1/logout-all`, `/v1/sessions` are one
    // concern from an operator's point of view.
    class_to_families.remove("session");

    let shared: Vec<_> = class_to_families
        .iter()
        .filter(|(_, families)| families.len() > 1)
        .collect();
    assert!(
        shared.is_empty(),
        "these metrics classes span more than one route family, so the label \
         cannot distinguish them: {shared:#?}"
    );
}

#[test]
fn the_metrics_class_of_a_path_is_bounded_and_never_the_raw_path() {
    // The classifier's whole reason to exist: a per-media-item time series
    // would be unbounded cardinality. Paths that differ only in their
    // identifier must collapse to one class, and an unknown path must not
    // leak its own text into the label.
    let get = salvo::http::Method::GET;
    let first = classify_route(&get, "/v1/media/11111111-1111-1111-1111-111111111111");
    let second = classify_route(&get, "/v1/media/22222222-2222-2222-2222-222222222222");
    assert_eq!(first, second, "two media items must share one route class");

    for path in [
        "/v1/definitely-not-a-route",
        "/v2/health",
        "/metrics",
        "/",
        "",
    ] {
        let class = classify_route(&get, path);
        assert_eq!(class, "other", "unknown path {path} must classify as other");
    }
}
