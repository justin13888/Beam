import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.jvm.toolchain.JavaLanguageVersion
import org.gradle.kotlin.dsl.configure
import org.jetbrains.kotlin.gradle.dsl.KotlinJvmProjectExtension

/**
 * A pure-Kotlin module with no Android dependency.
 *
 * `:core:model` uses this so the domain types cannot accidentally reach for
 * `android.*`, and so they stay usable from a plain JVM test with no
 * Robolectric sandbox in the way.
 */
class JvmLibraryConventionPlugin : Plugin<Project> {
    override fun apply(target: Project) = with(target) {
        pluginManager.apply("org.jetbrains.kotlin.jvm")

        extensions.configure<KotlinJvmProjectExtension> {
            jvmToolchain {
                languageVersion.set(JavaLanguageVersion.of(21))
            }
            // Public API must be explicit about visibility and return types:
            // this module is a vocabulary other modules depend on, and an
            // accidentally-public helper becomes someone else's dependency.
            explicitApi()
        }
    }
}
