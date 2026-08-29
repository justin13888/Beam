import com.android.build.api.dsl.ApplicationExtension
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.kotlin.dsl.configure
import org.gradle.kotlin.dsl.dependencies

/** Base configuration for the single application module. */
class AndroidApplicationConventionPlugin : Plugin<Project> {
    override fun apply(target: Project) = with(target) {
        pluginManager.apply("com.android.application")

        extensions.configure<ApplicationExtension> {
            configureKotlinAndroid(this)
            defaultConfig {
                targetSdk = version("targetSdk").toInt()
                versionCode = 1
                // Kept in step with the single product version (ADR-0009).
                versionName = "0.1.0"
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
