## Outcome

Kotlin gets the advisory mutation testing that NFR-210 and ADR-0011 give Rust, through one definitive mise task that a developer and a CI job both invoke. It never blocks a merge.

## Findings

- **android-TEST-023** `beam-android/gradle/libs.versions.toml`. Kotlin has no mutation testing. `grep -i pitest` across beam-android, mise.toml and .github finds nothing. `.github/workflows/mutants.yml` is Rust-only.

## Decisions taken

These are the run's decisions under the autonomy contract. Nobody has reviewed them.

1. Introduce Kotlin mutation testing.
   Taken: yes, as advisory tooling. This adds:
   - the `pl.droidsonroids.pitest` Gradle plugin, via the version catalog;
   - a `beam.android.pitest` convention plugin in build-logic, applied to `feature/*`, `core/media`, `core/ffi` and `app`, which excludes `uniffi.*`, Hilt/Dagger generated classes and `core/testing`;
   - a mise task `android:mutants` outside `ci`;
   - a `continue-on-error` job in `mutants.yml` that uploads `build/reports/pitest`.

   It is dev tooling, and no consumer of the app takes it on.
   Rejected: no Kotlin mutation testing. The Android client is the one the maintainer delivers, and its tests have the weakest constraint today.
   Reverses: remove the plugin, the convention, the task and the job.
2. The exclusion of `core/testing` doubles is enforced by the convention plugin's `excludedClasses`, the Kotlin analogue of `check:mutants-skip-fakes`.

## Surface

Internal: dev-only tooling.

## Constraints

- It is outside `mise run ci`, and the CI job is advisory and non-blocking.
- No gate is weakened.
- No feature is added.

Verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c. Found by the post-drain audit in #283.
