## Outcome

beam-android declares only the dependencies it imports. Every file over the agreed Kotlin threshold is split along a named boundary, and KDoc matches the code. Behaviour is unchanged. This is layer 2, precondition step 3.

## Findings

Each is `id path:line - finding - fix`.

- **android-IMPL-008** `beam-android/gradle/libs.versions.toml:104, 107-111, 118, 127-128` (versions at 45, 47, 49, 55). These catalog entries have no consumer: androidx-browser, paging-runtime, room-runtime/-ktx/-compiler, work-runtime-ktx, media3-ui-compose, androidx-hilt-work, androidx-hilt-compiler. Delete them.
- **android-IMPL-009** Module dependencies that are declared but never imported:
  - `core/ui/build.gradle.kts:17-18`: paging-compose, coil-compose
  - `core/model/build.gradle.kts:3, 7-8`: serialization plugin, serialization-json, collections-immutable
  - `AndroidComposeConventionPlugin.kt:40`: collections-immutable
  - `core/designsystem/build.gradle.kts:13-15, 17`: adaptive, adaptive-layout, adaptive-navigation, window-size
  - `app/build.gradle.kts:60`: glance-material3
  - `feature/player/build.gradle.kts:16`: media3-exoplayer

  Remove each one. Keep coil-network-okhttp: it loads through ServiceLoader, so an import grep cannot see it.
- **android-IMPL-011** `core/ffi/.../repository/Repositories.kt:184`. The `continueWatching` KDoc omits the next-episode rows. Correct it.
- **J8** `DeviceProfiles.kt:166-173`. `@Suppress("DEPRECATION")` covers `defaultDisplay.hdrCapabilities` on every API level. Branch on SDK version, as `displaySize` does (`:155-162`).
- **Splits** (Kotlin p90 is 312 lines):
  - `FakeRepositories.kt` (820): one file per fake.
  - `PlayerTest.kt`: `PlayerUpNextTest` and `PlayerSourceSwitchTest`.
  - `ExoBeamPlayer.kt`: `PlaybackFailures.kt` (601-652) and `TrackOptions.kt` (574-600).
  - `AuthViewModelTest.kt`: `AuthDeviceLoginTest` (240-473).
  - `AdminScreen.kt`: `AdminRows.kt` (289-457) and `ScanFormat.kt` (358-393).
  - `SettingsScreen.kt`: `SettingsLabels.kt` (413-433).
  - `AdminTest.kt`: `AdminFormatTest.kt`.

  These are cohesive and stay whole: `Fixtures.kt`, `DetailScreen.kt`, `AdminViewModel.kt`, `DownloadArtworkTest.kt`, `DetailTest.kt`, `PlayerViewModel.kt`, `Repositories.kt`.

Cursor paging (android-IMPL-002) and the duplicated core constants (android-IMPL-004..007) are delivered by the client-core implementation issue. The trust predicate (android-IMPL-001) and the progress flush (android-IMPL-003) are delivered by their defect issues.

## Decisions taken

These are the run's decisions under the autonomy contract. Nobody has reviewed them.

1. Removing unused Gradle dependencies.
   Taken: remove them. The app is not a library, so no consumer takes on its dependency set, and R8 already strips the unused code.
   Reverses: re-add the entries.

## Surface

Internal.

## Constraints

- Behaviour is preserved.
- No gate is weakened: `android:lint`, `android:test`, `android:screenshot` and `android:minify` must still pass.
- A change that would alter any other observable surface is not made; it is filed as a new issue.
- No feature is added.

Verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c. Found by the post-drain audit in #283.
