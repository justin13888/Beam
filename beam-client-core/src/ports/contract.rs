//! Shared behavioural contract for [`crate::ports::kv::KeyValueStore`].
//!
//! Instantiated over the in-memory double beside the trait, and meant for
//! every other implementation of it: the same assertions constraining the
//! double and the platform's store are what make the double legitimate, where
//! tests of the double alone proved only that a `BTreeMap` works.

/// The shared behavioural contract every [`crate::ports::kv::KeyValueStore`]
/// must meet.
///
/// `$make` is an expression producing a fresh, empty store; it is evaluated
/// once per case, and must implement
/// [`KeyValueStore`](crate::ports::kv::KeyValueStore) by value.
#[macro_export]
macro_rules! key_value_store_contract {
    ($make:expr) => {
        use $crate::ports::kv::KeyValueStore as _;

        fn key(name: &str) -> String {
            name.to_owned()
        }

        #[tokio::test]
        async fn a_written_value_reads_back_and_a_second_write_replaces_it() {
            let store = $make;
            store
                .put(key("servers/index"), key("[]"))
                .await
                .expect("put");
            assert_eq!(
                store.get(key("servers/index")).await.expect("get"),
                Some(key("[]"))
            );

            store
                .put(key("servers/index"), key("[\"a\"]"))
                .await
                .expect("put");
            assert_eq!(
                store.get(key("servers/index")).await.expect("get"),
                Some(key("[\"a\"]"))
            );
        }

        #[tokio::test]
        async fn an_absent_key_reads_as_none_and_removing_it_succeeds() {
            let store = $make;
            assert_eq!(store.get(key("nothing")).await.expect("get"), None);
            store.remove(key("nothing")).await.expect("remove");
            assert_eq!(
                store.get_secret(key("nothing")).await.expect("get_secret"),
                None
            );
            store
                .remove_secret(key("nothing"))
                .await
                .expect("remove_secret");
        }

        #[tokio::test]
        async fn a_removed_value_reads_as_none() {
            let store = $make;
            store.put(key("k"), key("v")).await.expect("put");
            store.remove(key("k")).await.expect("remove");
            assert_eq!(store.get(key("k")).await.expect("get"), None);
        }

        #[tokio::test]
        async fn a_secret_reads_back_is_replaced_and_is_removed() {
            let store = $make;
            store
                .put_secret(key("session/a"), key("first"))
                .await
                .expect("put_secret");
            store
                .put_secret(key("session/a"), key("second"))
                .await
                .expect("put_secret");
            assert_eq!(
                store
                    .get_secret(key("session/a"))
                    .await
                    .expect("get_secret"),
                Some(key("second"))
            );

            store
                .remove_secret(key("session/a"))
                .await
                .expect("remove_secret");
            assert_eq!(
                store
                    .get_secret(key("session/a"))
                    .await
                    .expect("get_secret"),
                None
            );
        }

        /// The whole point of the split boundary: a secret is not reachable
        /// through the plaintext methods, and a plaintext value is not
        /// reachable through the secret ones, under the same key.
        #[tokio::test]
        async fn secrets_and_plaintext_are_separate_namespaces() {
            let store = $make;
            store
                .put_secret(key("session/a"), key("cookie"))
                .await
                .expect("put_secret");
            assert_eq!(store.get(key("session/a")).await.expect("get"), None);

            store
                .put(key("servers/a"), key("record"))
                .await
                .expect("put");
            assert_eq!(
                store
                    .get_secret(key("servers/a"))
                    .await
                    .expect("get_secret"),
                None
            );

            // Removing one never removes the other.
            store.put(key("both"), key("plain")).await.expect("put");
            store
                .put_secret(key("both"), key("secret"))
                .await
                .expect("put_secret");
            store.remove(key("both")).await.expect("remove");
            assert_eq!(
                store.get_secret(key("both")).await.expect("get_secret"),
                Some(key("secret"))
            );
        }

        #[tokio::test]
        async fn list_keys_is_every_live_plaintext_key_with_the_prefix() {
            let store = $make;
            store.put(key("servers/a"), key("1")).await.expect("put");
            store.put(key("servers/b"), key("2")).await.expect("put");
            store.put(key("servers/gone"), key("3")).await.expect("put");
            store.remove(key("servers/gone")).await.expect("remove");
            store.put(key("pins/a"), key("4")).await.expect("put");
            store
                .put_secret(key("servers/secret"), key("s"))
                .await
                .expect("put_secret");

            let mut keys = store.list_keys(key("servers/")).await.expect("list_keys");
            keys.sort();
            assert_eq!(keys, vec![key("servers/a"), key("servers/b")]);
            assert!(
                store
                    .list_keys(key("absent/"))
                    .await
                    .expect("list_keys")
                    .is_empty()
            );
        }
    };
}
