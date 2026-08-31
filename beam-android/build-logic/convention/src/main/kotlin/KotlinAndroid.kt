import com.android.build.api.dsl.CommonExtension
import org.gradle.api.JavaVersion
import org.gradle.api.Project
import org.gradle.api.artifacts.VersionCatalogsExtension
import org.gradle.kotlin.dsl.getByType
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.KotlinAndroidProjectExtension

/** The shared version catalog, reachable from a plugin rather than a script. */
internal val Project.libs
    get() = extensions.getByType<VersionCatalogsExtension>().named("libs")

internal fun Project.version(alias: String): String = libs.findVersion(alias).get().requiredVersion

/**
 * The Android and Kotlin settings every module shares.
 *
 * Applied from one place so a compile SDK bump is a single-line change rather
 * than fifteen, which is the same argument ADR-0009 makes for commands.
 *
 * Written as property access rather than the familiar `defaultConfig { }`
 * blocks: AGP 9's `CommonExtension` exposes these as plain getters and no
 * longer offers the Action-taking overloads the block syntax compiles to.
 */
internal fun Project.configureKotlinAndroid(extension: CommonExtension) {
    extension.compileSdk = version("compileSdk").toInt()
    // The SDK now ships minor platform revisions, and the installed platform
    // is android-37.2, so compileSdk alone does not identify it.
    extension.compileSdkMinor = version("compileSdkMinor").toInt()
    extension.defaultConfig.minSdk = version("minSdk").toInt()

    // Keeps java.time and friends usable at minSdk 26 without every call site
    // having to care.
    extension.compileOptions.isCoreLibraryDesugaringEnabled = true
    // AGP still defaults the Java tasks to 11, and Kotlin below targets 21.
    // A mismatch is a hard error rather than a warning, so both are set here
    // instead of being discovered once per module.
    extension.compileOptions.sourceCompatibility = JavaVersion.VERSION_21
    extension.compileOptions.targetCompatibility = JavaVersion.VERSION_21

    extension.packaging.resources.excludes +=
        setOf(
            "/META-INF/{AL2.0,LGPL2.1}",
            "/META-INF/versions/9/previous-compilation-data.bin",
        )

    extension.testOptions.unitTests.isIncludeAndroidResources = true

    extension.lint.warningsAsErrors = true
    extension.lint.checkDependencies = true
    // Dependency freshness is dependabot's job, not the build's: a lint
    // failure on a day-old release would block unrelated work.
    extension.lint.disable += setOf("GradleDependency", "AndroidGradlePluginVersion")

    extensions.configure(KotlinAndroidProjectExtension::class.java) {
        compilerOptions {
            jvmTarget.set(JvmTarget.JVM_21)
            freeCompilerArgs.addAll("-opt-in=kotlin.RequiresOptIn")
        }
    }
}
