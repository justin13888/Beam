import com.android.build.api.dsl.CommonExtension
import org.gradle.api.JavaVersion
import org.gradle.api.Project
import org.gradle.api.artifacts.VersionCatalogsExtension
import org.gradle.api.file.FileCollection
import org.gradle.api.tasks.InputFiles
import org.gradle.api.tasks.PathSensitive
import org.gradle.api.tasks.PathSensitivity
import org.gradle.kotlin.dsl.getByType
import org.gradle.process.CommandLineArgumentProvider
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
    configureRobolectricOffline(extension)

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

/**
 * Runs Robolectric against an Android jar Gradle resolved, never one it
 * downloads itself.
 *
 * Left alone, Robolectric fetches its preinstrumented `android-all` jar from
 * Maven Central when the first test starts, with one HTTP request and no
 * retry, outside Gradle's resolver and cache. A transient 404 then fails a
 * module the change never touched (#391). Here the jar is a dependency of a
 * dedicated configuration instead, so it arrives through Gradle's retrying
 * resolver and its cache, and the tests run with `robolectric.offline=true`:
 * a test makes no network request of its own, and a jar that does not match
 * the pinned SDK fails loudly instead of being fetched.
 *
 * Applied to every Android module rather than to the ones that use Robolectric
 * today, so a module that starts using it is covered without remembering to
 * opt in. A module with no Robolectric test ignores the two properties, and the
 * jar is resolved once into the shared Gradle cache rather than per module.
 */
private fun Project.configureRobolectricOffline(extension: CommonExtension) {
    val androidAll =
        configurations.create("robolectricAndroidAll") {
            description = "The preinstrumented Android jar Robolectric runs the unit tests against."
            isCanBeConsumed = false
            isCanBeResolved = true
            // A single self-contained jar: its POM declares no dependencies,
            // and anything it did declare Robolectric would not read anyway.
            isTransitive = false
        }
    dependencies.add(
        androidAll.name,
        libs.findLibrary("robolectric.android.all.instrumented").get(),
    )

    extension.testOptions.unitTests.all { test ->
        test.systemProperty("robolectric.offline", "true")
        test.jvmArgumentProviders.add(RobolectricDependencyDir(androidAll))
    }
}

/**
 * Points `robolectric.dependency.dir` at the directory Gradle resolved the
 * Android jar into.
 *
 * Robolectric's offline resolver looks for `<artifactId>-<version>.jar` in that
 * one directory, which is exactly the name Gradle's cache stores it under, so
 * the cache entry is used in place rather than copied into every module's build
 * directory (the jar is over 200 MB). The jar is the task input, fingerprinted
 * by name only; the absolute directory is not, so the test task stays
 * relocatable for the build cache.
 */
internal class RobolectricDependencyDir(
    @get:InputFiles
    @get:PathSensitive(PathSensitivity.NAME_ONLY)
    val androidAll: FileCollection,
) : CommandLineArgumentProvider {
    override fun asArguments(): Iterable<String> {
        val directory =
            androidAll.files
                .map { it.parentFile }
                .distinct()
                .singleOrNull()
                ?: error("robolectricAndroidAll must resolve to one jar, got ${androidAll.files}")
        return listOf("-Drobolectric.dependency.dir=${directory.absolutePath}")
    }
}
