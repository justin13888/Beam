import com.android.build.api.dsl.LibraryExtension
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.kotlin.dsl.configure
import org.gradle.kotlin.dsl.dependencies
import org.gradle.kotlin.dsl.project

/**
 * Everything a `:feature:*` module needs, so a feature's own build file says
 * only what is unusual about it.
 *
 * Features depend on `:core:*` and never on each other. Cross-feature
 * navigation travels as a lambda from the nav host rather than as a module
 * dependency, which is what keeps the graph a tree and lets one feature be
 * tested without dragging in nine others.
 */
class AndroidFeatureConventionPlugin : Plugin<Project> {
    override fun apply(target: Project) = with(target) {
        pluginManager.apply("beam.android.library")
        pluginManager.apply("beam.android.compose")
        pluginManager.apply("beam.android.hilt")
        pluginManager.apply("org.jetbrains.kotlin.plugin.serialization")

        extensions.configure<LibraryExtension> {
            testOptions.unitTests.all { test ->
                // Robolectric's native graphics mode renders real pixels,
                // which is what makes the screenshot tests meaningful rather
                // than a record of blank rectangles.
                test.systemProperty("robolectric.graphicsMode", "NATIVE")
            }
        }

        dependencies {
            add("implementation", project(":core:model"))
            add("implementation", project(":core:ffi"))
            add("implementation", project(":core:designsystem"))
            add("implementation", project(":core:ui"))

            addAll(
                libs,
                "implementation",
                "androidx.lifecycle.viewmodel.compose",
                "androidx.hilt.navigation.compose",
                "androidx.navigation3.runtime",
                "androidx.navigation3.ui",
                "kotlinx.serialization.json",
            )

            add("testImplementation", project(":core:testing"))
        }
    }
}
