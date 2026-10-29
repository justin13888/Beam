## Outcome

beam-android's tests reach the real repository adapters, the UniFFI bindings over the host-built `libbeam_client_core`, the DataStore-backed stores, device-profile construction and the player. The fakes behave like the real core, so view-model tests prove what a phone does. This is the characterization net the Android implementation slice relies on (layer 3, step 2).

## Findings

Each is `id path:line - finding - test`.

- **android-TEST-005** `beam-android/core/ffi/src/main/kotlin/dev/beam/android/core/ffi/repository/BeamRepositories.kt:50`. No test loads the native library or exercises a `Beam*Repository` adapter. The republish rules are unconstrained: there is no emission before `restore`, and `trustCertificate` does not republish. Add a JVM test that loads the host `libbeam_client_core.so` built by `core:bindgen` (mise.toml:1036) via `jna.library.path`: `the_registry_round_trips_through_the_bindings_and_datastore`.
- **android-TEST-006** `core/media/.../player/ExoBeamPlayer.kt:84` (652 lines) is untested and constructs `RebufferMonitor()` directly. Inject the monitor, then add Robolectric and Media3 `TestExoPlayerBuilder` tests: `pausing_forces_a_progress_report` and `a_plan_that_cannot_be_built_is_a_playback_error`.
- **android-TEST-007** `feature/auth/.../AuthViewModelTest.kt:77,100,130,151`. These tests inject failures the real core cannot raise at those calls: `login_url` and `add_server` are local. Drive them through `startDeviceLogin` or `completeLogin` instead.
- **android-TEST-008** `core/testing/.../FakeRepositories.kt:336-361, 461-466`. `FakeServerRepository` diverges from the real core. It starts with seeded servers, `addServer` is not idempotent, `logout` ignores `failWith`, and a 401 never changes state. Align the fake, then add `adding_the_same_address_twice_lists_one_server`.
- **android-TEST-010 (shared contract)** Run the `KeyValueStore` contract from the client-core test issue over `DataStoreKeyValueStore` (Robolectric).
- **android-TEST-011 / -012** `PreferencesRepository.kt:27`, `DataStoreKeyValueStore.kt:87`, `SecretCipher`. Untested: the DataStore round trip, the fallback for an unknown enum, the comma-joined list, `guard` mapping to `StorageException.Unavailable`, `listKeys` excluding secrets, and `decrypt` returning null on garbage. Add `an_unknown_stored_theme_falls_back_to_system`, `preferences_survive_a_new_instance` and `a_corrupt_secret_reads_as_absent`.
- **android-TEST-013** `core/ffi/.../DeviceProfiles.kt:47`. Profile construction from `MediaCodecList` is untested. Add `ShadowMediaCodecList` tests `a_main10_hevc_decoder_reports_10_bit` and `an_encoder_is_never_a_decoder`.
- **android-TEST-014** `feature/explore/.../ExploreViewModelTest.kt:50`. The debounce test would pass with no debounce. Space the keystrokes with `advanceTimeBy(299)`.
- **android-TEST-016** These tests assert only `isNotEmpty()`: `HomeViewModelTest.kt:38`, `HistoryTest.kt:32`, `AdminTest.kt:38`, `DetailTest.kt:62`, `LibrariesTest.kt:35` and `AuthViewModelTest.kt:39`. Assert the fake's rows and their order.
- **android-TEST-017** `DetailTest.kt:103` mirrors the `asPolicy` table; delete it. `SettingsTest.kt:249` barely constrains its subject.
- **android-TEST-018** `core/ui/.../Format.kt:12` is untested. Add a table-driven `FormatTest` covering the boundaries.
- **android-TEST-020** `app/.../ui/BeamApp.kt:62`. There are no navigation or Compose interaction tests. Add `signing_out_resets_the_back_stack_to_sign_in` and `a_destination_round_trips_through_saved_state`.
- **android-TEST-021** There are two `FakeDownloadRepository`s (feature/detail and feature/downloads). The KDoc justifies keeping Media3 off other classpaths, so keep both, but share the common parts if that is possible without the dependency.
- **android-TEST-022** `ContinueWatchingWidget.kt` and `PlaybackService.kt` are untested. Add `the_widget_lists_continue_watching_rows` (Glance).
- **android-IMPL-010** `core/testing/.../Fixtures.kt:201` re-derives `progressFraction` and diverges from the core at duration 0. Use a literal.

The cold-start and mid-use session tests (android-TEST-001, -002), the progress flush (-003), media origin scoping (-004), and the settings and admin error reporting (-009, -010, -015, -019) are delivered by their own defect issues.

## Decisions taken

These are the run's decisions under the autonomy contract. Nobody has reviewed them.

1. Loading the native core in JVM tests.
   Taken: use the host `.so` that `core:bindgen` already builds. Add a Gradle test dependency on that task's output, and skip with a failing guard (never silently) when it is missing in CI.
   Rejected: Robolectric fakes of the bindings. Those prove only the fakes.
   Reverses: drop the native test source set.

## Surface

Internal: tests, test fakes, and one injectable constructor parameter on `ExoBeamPlayer` (internal to the module).

## Constraints

- Behaviour is preserved.
- No gate is weakened.
- A change that would alter any other observable surface is not made; it is filed as a new issue.
- No feature is added.

Verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c. Found by the post-drain audit in #283.
