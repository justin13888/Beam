import com.android.build.api.dsl.ApplicationExtension
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.kotlin.dsl.configure
import org.gradle.kotlin.dsl.dependencies

/** Base configuration for the single application module. */
class AndroidApplicationConventionPlugin : Plugin<Project> {
    override fun apply(target: Project) =
        with(target) {
            pluginManager.apply("com.android.application")

            extensions.configure<ApplicationExtension> {
                configureKotlinAndroid(this)
                defaultConfig {
                    targetSdk = version("targetSdk").toInt()
                    // Read from version.txt rather than restated here. That is
                    // the file release-please's `simple` strategy already
                    // rewrites, so the app's version follows the single product
                    // version (ADR-0009) with nothing to keep in step by hand
                    // and no second place to forget.
                    val productVersion = productVersion()
                    versionName = productVersion
                    versionCode = versionCodeFor(productVersion)
                }

                buildTypes {
                    release {
                        isMinifyEnabled = true
                        isShrinkResources = true
                        proguardFiles(
                            getDefaultProguardFile("proguard-android-optimize.txt"),
                            "proguard-rules.pro",
                        )
                    }
                    debug {
                        // Both build types are installable side by side, so a
                        // debug build never overwrites a release one on a device
                        // being used to compare them.
                        applicationIdSuffix = ".debug"
                        versionNameSuffix = "-debug"
                    }
                }

                // Generated once per locale from the resources actually present,
                // so per-app language selection lists exactly what is translated.
                androidResources.generateLocaleConfig = true
            }

            dependencies {
                add("coreLibraryDesugaring", libs.findLibrary("android.desugarJdkLibs").get())
            }
        }
}

/** The single product version, from `version.txt` at the repository root. */
private fun Project.productVersion(): String =
    rootProject.layout.projectDirectory.asFile.parentFile
        .resolve("version.txt")
        .readText()
        .trim()

/**
 * A monotonically increasing integer derived from the semantic version.
 *
 * Android orders installs by `versionCode` alone and refuses a downgrade, so a
 * constant would make every build after the first unupgradable on a device --
 * and Play rejects an upload whose code has not increased. Encoding the
 * version as `major * 10000 + minor * 100 + patch` keeps the ordering the
 * semantic version already implies, and allows 100 patches and 100 minors per
 * step, which is far beyond anything this project will reach.
 *
 * A pre-release suffix is ignored deliberately: `1.2.0-rc.1` and `1.2.0` share
 * a code, so a release candidate can be replaced in place by the release.
 */
private fun versionCodeFor(version: String): Int {
    val parts = version.substringBefore('-').split('.')
    require(parts.size == 3) { "version.txt should hold a semantic version, found: $version" }
    val (major, minor, patch) = parts.map { it.toInt() }
    return major * 10_000 + minor * 100 + patch
}
