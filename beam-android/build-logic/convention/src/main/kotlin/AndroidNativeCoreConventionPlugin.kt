import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.api.tasks.PathSensitivity
import org.gradle.api.tasks.testing.Test
import org.gradle.kotlin.dsl.configure
import org.gradle.kotlin.dsl.dependencies
import org.gradle.kotlin.dsl.withType
import pl.droidsonroids.gradle.pitest.PitestPluginExtension

/**
 * Lets a module's JVM unit tests call into the real `beam-client-core`.
 *
 * The screens page through the core's `CursorPager`, ask a core error whether
 * it is retryable, and name a resolution with the core's rule (issue #396), so
 * a test of them is a test of the core's answer -- never of a Kotlin copy of
 * it. The host library is the one `:core:ffi:hostCoreLibrary` builds for this
 * machine (issue #355); `:core:ffi` wires its own tests to it, and every other
 * module that reaches the core applies this.
 *
 * JNA searches `jna.library.path` before the system's, so the bindings find
 * that library and only that one.
 */
class AndroidNativeCoreConventionPlugin : Plugin<Project> {
    override fun apply(target: Project) =
        with(target) {
            // The task is registered by `:core:ffi`'s own build script, so that
            // script has to have run before its output can be named here.
            evaluationDependsOn(CORE_FFI)
            val hostCoreLibrary = project(CORE_FFI).tasks.named(HOST_CORE_LIBRARY)
            val library = hostCoreLibrary.map { it.outputs.files.singleFile }

            tasks.withType<Test>().configureEach {
                inputs
                    .files(hostCoreLibrary)
                    .withPropertyName("hostCoreLibrary")
                    .withPathSensitivity(PathSensitivity.NONE)
                systemProperty("jna.library.path", library.get().absolutePath)
            }

            // PIT runs the same tests in JVMs of its own, which see none of the
            // Test task's system properties.
            pluginManager.withPlugin("pl.droidsonroids.pitest") {
                extensions.configure<PitestPluginExtension> {
                    jvmArgs.add(library.map { "-Djna.library.path=${it.absolutePath}" })
                }
                tasks.matching { it.name.startsWith("pitest") }.configureEach { dependsOn(hostCoreLibrary) }
            }

            dependencies {
                // The plain jar, for the desktop `jnidispatch` it carries: the
                // AAR `:core:ffi` depends on packages JNA's dispatcher for
                // Android ABIs only.
                add("testImplementation", libs.findLibrary("jna").get())
            }
        }

    private companion object {
        const val CORE_FFI = ":core:ffi"
        const val HOST_CORE_LIBRARY = "hostCoreLibrary"
    }
}
