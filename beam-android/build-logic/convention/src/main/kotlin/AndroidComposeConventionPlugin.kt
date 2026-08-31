import com.android.build.api.dsl.CommonExtension
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.api.artifacts.VersionCatalog
import org.gradle.kotlin.dsl.dependencies

/**
 * Compose configuration, shared by every module that renders anything.
 *
 * The Compose compiler ships with Kotlin itself since 2.0, so there is no
 * separate compiler version to keep in step with the runtime -- one less pin
 * that can silently disagree.
 */
class AndroidComposeConventionPlugin : Plugin<Project> {
    override fun apply(target: Project) =
        with(target) {
            pluginManager.apply("org.jetbrains.kotlin.plugin.compose")

            val extension =
                extensions.findByName("android") as? CommonExtension
                    ?: error("beam.android.compose requires an Android module")
            extension.buildFeatures.compose = true

            dependencies {
                val bom = libs.findLibrary("androidx.compose.bom").get()
                add("implementation", platform(bom))
                add("androidTestImplementation", platform(bom))
                add("testImplementation", platform(bom))

                addAll(
                    libs,
                    "implementation",
                    "androidx.compose.foundation",
                    "androidx.compose.ui",
                    "androidx.compose.ui.graphics",
                    "androidx.compose.ui.tooling.preview",
                    "androidx.compose.material3",
                    "androidx.compose.material.icons.extended",
                    "androidx.lifecycle.runtime.compose",
                    "kotlinx.collections.immutable",
                )
                // Tooling is debug-only: it drags the layout inspector and preview
                // infrastructure in, none of which belongs in a release APK.
                add("debugImplementation", libs.findLibrary("androidx.compose.ui.tooling").get())
            }
        }
}

internal fun org.gradle.api.artifacts.dsl.DependencyHandler.addAll(
    libs: VersionCatalog,
    configuration: String,
    vararg aliases: String,
) {
    aliases.forEach { alias -> add(configuration, libs.findLibrary(alias).get()) }
}
