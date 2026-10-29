import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.kotlin.dsl.configure
import pl.droidsonroids.gradle.pitest.PitestPluginExtension

/**
 * PIT mutation testing over a module's JVM unit tests.
 *
 * Advisory, exactly as cargo-mutants is for the Rust workspace (ADR-0011): the
 * `android:mutants` mise task that drives it is outside `mise run ci`, and the
 * job in `.github/workflows/mutants.yml` is `continue-on-error`. A surviving
 * mutant is a prompt to harden the code, never a broken build -- so no
 * `mutationThreshold` is set here, and none should be.
 *
 * Apply it after the Android plugin (`beam.android.library`,
 * `beam.android.feature` or `beam.android.application`): the PIT plugin
 * registers one `pitest<Variant>` task per variant it finds. The mise task runs
 * only `pitestDebug`, the variant every other unit-test gate uses.
 */
class AndroidPitestConventionPlugin : Plugin<Project> {
    override fun apply(target: Project) =
        with(target) {
            pluginManager.apply("pl.droidsonroids.pitest")

            extensions.configure<PitestPluginExtension> {
                // Pinned in the catalog so a plugin bump cannot silently move
                // the engine, and dependabot sees both.
                pitestVersion.set(version("pitest"))

                // Only Beam's own classes are mutation targets. Without this
                // the plugin derives the target from the project group, which
                // these modules never set.
                targetClasses.set(setOf("dev.beam.android.*"))

                // The packages outside `dev.beam.android` below are already
                // outside the target glob; they are listed so that widening it
                // cannot pull generated code back in.
                excludedClasses.set(
                    setOf(
                        // UniFFI's generated Kotlin bindings for
                        // beam-client-core. A mutant there is a mutant of
                        // generated FFI glue; the Rust side is what
                        // cargo-mutants covers.
                        "uniffi.*",
                        // Hilt and Dagger output: components, factories,
                        // injectors and the aggregated-deps markers. None of it
                        // is written by hand. The hand-written @Module classes
                        // under `di` packages stay targets: they carry real
                        // decisions (ExoPlayer's decoder fallback, the image
                        // loader's signed-out path), and only the factories
                        // Dagger generates from them are excluded here.
                        "hilt_aggregated_deps.*",
                        "dagger.*",
                        "*.Hilt_*",
                        "*_HiltModules*",
                        "*_HiltComponents*",
                        "*_ComponentTreeDeps",
                        "*_GeneratedInjector",
                        "*_Factory",
                        "*_MembersInjector",
                        // Test scaffolding. :core:testing's fakes are on every
                        // feature's test classpath and match the target glob,
                        // but they are doubles -- never the subject (see
                        // "Never test the double" in CLAUDE.md).
                        "dev.beam.android.core.testing.*",
                        // Compiler output with no Beam logic in it.
                        "*.BuildConfig",
                        "*.R",
                        "*.R$*",
                        "*.ComposableSingletons*",
                    ),
                )

                // A module with no tests, or with only screenshot tests, has
                // nothing to kill a mutant with. That is a coverage fact, not a
                // reason to fail the advisory run.
                failWhenNoMutations.set(false)

                // Reports land at a stable path so the CI job can upload them
                // and a rerun overwrites rather than accumulates.
                timestampedReports.set(false)
                outputFormats.set(setOf("HTML", "XML"))

                threads.set(Runtime.getRuntime().availableProcessors())
            }
        }
}
