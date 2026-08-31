import com.android.build.api.dsl.CommonExtension
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.kotlin.dsl.dependencies

/**
 * Roborazzi screenshot testing, on the JVM.
 *
 * Screenshot tests run under Robolectric rather than on a device, which is the
 * only reason they can be a required CI check: an emulator in CI is slow,
 * flaky, and needs nested virtualisation that hosted runners do not reliably
 * have. Robolectric's native graphics mode renders real pixels, so the images
 * are a genuine record of what the app draws rather than of blank rectangles.
 */
class AndroidScreenshotConventionPlugin : Plugin<Project> {
    override fun apply(target: Project) =
        with(target) {
            pluginManager.apply("io.github.takahirom.roborazzi")

            val extension =
                extensions.findByName("android") as? CommonExtension
                    ?: error("beam.android.screenshot requires an Android module")

            // Robolectric needs the merged resources and the manifest, and
            // without this every screenshot test fails at startup rather than
            // producing a useful diff.
            extension.testOptions.unitTests.isIncludeAndroidResources = true

            // One shared `robolectric.properties` rather than a copy per module:
            // the API level it pins is a property of the Robolectric version,
            // not of any one module, and a per-module copy would drift.
            extension.sourceSets.getByName("test").resources.srcDir(
                rootProject.layout.projectDirectory.dir("gradle/robolectric"),
            )

            // Shared with every other module for the same reason: the pinned
            // API level belongs to the Robolectric version, not to a module.
            extension.sourceSets.getByName("test").resources.srcDir(
                rootProject.layout.projectDirectory.dir("gradle/robolectric"),
            )

            extension.testOptions.unitTests.all { test ->
                test.systemProperty("robolectric.graphicsMode", "NATIVE")
                // Written next to the sources rather than under `build/`, so
                // the references survive a clean and are reviewable in a diff.
                // Written relative to the module directory, so references land
                // in `src/test/screenshots` and are reviewable in a diff. The
                // default puts them under `build/`, where they do not survive a
                // clean and cannot be compared in a pull request.
                test.systemProperty(
                    "roborazzi.record.filePathStrategy",
                    "relativePathFromCurrentDirectory",
                )
            }

            dependencies {
                addAll(
                    libs,
                    "testImplementation",
                    "roborazzi",
                    "roborazzi.compose",
                    "roborazzi.junit.rule",
                    "robolectric",
                    "junit",
                    "androidx.compose.ui.test.junit4",
                )
                add(
                    "debugImplementation",
                    libs.findLibrary("androidx.compose.ui.test.manifest").get(),
                )
            }
        }
}
