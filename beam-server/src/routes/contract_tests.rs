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

    use kynos::openapi::SpecVersion;

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

    /// The router describes itself, with nothing merely tolerated.
    ///
    /// `validate` reports every violation including warnings: a duplicated
    /// `operationId`, two paths differing only in the name of a variable, a
    /// security requirement naming a scheme that was never declared. A router
    /// that cannot describe itself never reaches a listener in `main`, so this
    /// is the same check the process makes at startup -- run without one.
    #[test]
    fn the_router_describes_itself_without_violations() {
        let violations = create_router()
            .validate()
            .expect("the router is describable");

        assert!(
            violations.is_empty(),
            "the router describes itself with violations: {violations:#?}"
        );
    }

    /// The 3.2 export the codegen tasks run cannot fail.
    ///
    /// `mise run codegen:openapi` writes this document and
    /// `codegen:openapi:check` diffs it, so an export that errors breaks a CI
    /// gate rather than a test. Asserting it here names the cause instead.
    #[test]
    fn the_contract_exports_as_openapi_3_2() {
        create_router()
            .openapi_as(SpecVersion::V3_2)
            .expect("the contract exports as OpenAPI 3.2");
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
        let exported = create_router()
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
