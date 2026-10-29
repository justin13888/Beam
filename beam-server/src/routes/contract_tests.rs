//! The router-to-description contract, asserted rather than mirrored.
//!
//! Under Salvo this file parsed the router's `Debug` box-drawing to compare two
//! independent passes -- a route tree and a `merge_router` document -- that
//! could disagree. Kynos derives both from one walk of `create_router`, so that
//! comparison is now a tautology and the parser that made it is deleted.
//!
//! What is *not* a tautology is below: that the router describes itself with no
//! violations, and that the operation set it describes is the one the committed
//! contract was generated from. The second is the readiness contract's "every
//! registered public endpoint appears exactly once in the exported
//! specification" (ADR-0010), and it is the assertion that turns a route added
//! without regenerating the clients into a failing test rather than a silent
//! contract change.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use kynos::openapi::{SpecError, SpecVersion};

    use crate::bootstrap::AllowedOrigins;
    use crate::config::ServerConfig;
    use crate::routes::create_router;

    /// Every method the OpenAPI Path Item Object can carry.
    const METHODS: [&str; 7] = ["get", "put", "post", "delete", "patch", "head", "options"];

    /// The `(path, method)` pairs a document describes.
    fn operations(document: &serde_json::Value) -> BTreeSet<(String, String)> {
        document["paths"]
            .as_object()
            .expect("paths is an object")
            .iter()
            .flat_map(|(path, item)| {
                item.as_object()
                    .expect("a path item is an object")
                    .keys()
                    .filter(|method| METHODS.contains(&method.as_str()))
                    .map(move |method| (path.clone(), method.clone()))
            })
            .collect()
    }

    /// The schemas Beam leaves unconstrained on purpose, by location.
    ///
    /// Each is an `Unchecked<_>` whose field documents why no schema is true of
    /// it. Kynos reports every one as a warning so that none is taken silently;
    /// listing them here is what makes adding another a reviewed change rather
    /// than a new warning nobody reads.
    const UNCHECKED_SCHEMAS: [&str; 1] =
        ["#/components/schemas/AdminLogEntry/properties/details/anyOf/0"];

    /// The router describes itself, with nothing merely tolerated beyond the
    /// waivers in [`UNCHECKED_SCHEMAS`].
    ///
    /// `validate` reports every violation including warnings: a duplicated
    /// `operationId`, two paths differing only in the name of a variable, a
    /// security requirement naming a scheme that was never declared. A router
    /// that cannot describe itself never reaches a listener in `main`, so this
    /// is the same check the process makes at startup -- run without one.
    #[test]
    fn the_router_describes_itself_without_violations() {
        let violations = create_router(&AllowedOrigins::none())
            .validate()
            .expect("the router is describable");

        let (unchecked, other): (Vec<_>, Vec<_>) = violations
            .into_iter()
            .partition(|violation| violation.error == SpecError::UncheckedSchema);

        assert!(
            other.is_empty(),
            "the router describes itself with violations: {other:#?}"
        );
        assert_eq!(
            unchecked
                .iter()
                .map(|violation| violation.location.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(UNCHECKED_SCHEMAS),
            "the unconstrained schemas are exactly the ones waived on purpose"
        );
    }

    /// The 3.2 export the codegen tasks run cannot fail.
    ///
    /// `mise run codegen:openapi` writes this document and
    /// `codegen:openapi:check` diffs it, so an export that errors breaks a CI
    /// gate rather than a test. Asserting it here names the cause instead.
    #[test]
    fn the_contract_exports_as_openapi_3_2() {
        create_router(&AllowedOrigins::none())
            .openapi_as(SpecVersion::V3_2)
            .expect("the contract exports as OpenAPI 3.2");
    }

    /// The CORS allow-list is the one thing `create_router` takes, and it is
    /// a dispatch-time policy: the document a configured server serves is the
    /// one the export commits, whatever origins the deployment names.
    #[test]
    fn the_document_does_not_depend_on_the_allowed_origins() {
        let document = |origins: &AllowedOrigins| {
            serde_json::to_value(
                create_router(origins)
                    .openapi_as(SpecVersion::V3_2)
                    .expect("the contract exports"),
            )
            .expect("the document serializes")
        };

        let configured = AllowedOrigins::from_config(&ServerConfig {
            web_url: "https://beam.example.com".to_owned(),
            extra_allowed_origins: Some("https://other.example.com".to_owned()),
            ..Default::default()
        });

        assert_eq!(document(&AllowedOrigins::none()), document(&configured));
    }

    /// No operation describes a transport header as one of its parameters
    /// (issue #200).
    ///
    /// The headers a browser or HTTP stack sets on its own -- the ones
    /// `EnforceSameOrigin` and the session record read, plus the forwarding
    /// fields the trusted-proxy policy reads -- are not inputs a client
    /// chooses, and describing them puts an argument for each in every
    /// generated client. The names come from the groups' own `NAMES`, so a
    /// header added to either group is covered without touching this test.
    #[test]
    fn no_operation_describes_a_transport_header() {
        use kynos::extract::params::header::HeaderParams;

        use crate::routes::auth::ClientHeaders;
        use crate::routes::middleware::OriginHeaders;

        let transport: BTreeSet<String> = OriginHeaders::NAMES
            .iter()
            .chain(ClientHeaders::NAMES)
            .chain(&["Forwarded", "X-Forwarded-For", "X-Real-IP"])
            .map(|name| name.to_ascii_lowercase())
            .collect();

        let exported = create_router(&AllowedOrigins::none())
            .openapi_as(SpecVersion::V3_2)
            .expect("the contract exports");
        let exported = serde_json::to_value(&exported).expect("the document serializes");

        let described: BTreeSet<(String, String, String)> = exported["paths"]
            .as_object()
            .expect("paths is an object")
            .iter()
            .flat_map(|(path, item)| {
                item.as_object()
                    .expect("a path item is an object")
                    .iter()
                    .filter(|(method, _)| METHODS.contains(&method.as_str()))
                    .flat_map(move |(method, operation)| {
                        operation["parameters"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|parameter| parameter["in"] == "header")
                            .filter_map(|parameter| parameter["name"].as_str())
                            .map(move |name| (path.clone(), method.clone(), name.to_owned()))
                    })
            })
            .filter(|(_, _, name)| transport.contains(&name.to_ascii_lowercase()))
            .collect();

        assert!(
            described.is_empty(),
            "transport headers described as operation parameters: {described:#?}"
        );
    }

    /// The exported operation set is the one the committed contract carries.
    ///
    /// Compared against `beam-web/openapi.json`, which is committed and is what
    /// every generated client is built from. Mounting a route without
    /// regenerating it fails here, which is the point: the alternative is a
    /// server that serves an operation no client knows about.
    ///
    /// The *set*, not the whole document -- `codegen:openapi:check` already
    /// diffs the bytes, and repeating that here would be a second copy of the
    /// same assertion that drifts on its own schedule.
    #[test]
    fn the_exported_operation_set_matches_the_committed_contract() {
        let exported = create_router(&AllowedOrigins::none())
            .openapi_as(SpecVersion::V3_2)
            .expect("the contract exports");
        let exported = serde_json::to_value(&exported).expect("the document serializes");

        let committed: serde_json::Value =
            serde_json::from_str(include_str!("../../../beam-web/openapi.json"))
                .expect("the committed contract parses");

        let (exported, committed) = (operations(&exported), operations(&committed));

        assert_eq!(
            exported,
            committed,
            "the router and the committed contract describe different operations.\n\
             only in the router: {:#?}\n\
             only in the committed contract: {:#?}\n\
             run: mise run codegen:openapi",
            exported.difference(&committed).collect::<Vec<_>>(),
            committed.difference(&exported).collect::<Vec<_>>(),
        );
    }
}
