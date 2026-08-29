import com.android.build.api.dsl.LibraryExtension
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.kotlin.dsl.configure
import org.gradle.kotlin.dsl.dependencies


/** Base configuration for every Android library module. */
class AndroidLibraryConventionPlugin : Plugin<Project> {
    override fun apply(target: Project) = with(target) {
        pluginManager.apply("com.android.library")

        extensions.configure<LibraryExtension> {
            configureKotlinAndroid(this)
            // A library declares no targetSdk -- only the application does,
            // and AGP 9 removed the property from the library DSL accordingly.
            //
            // No module needs BuildConfig by default, and generating one per
            // module is a needless compilation unit in every incremental build.
            buildFeatures.buildConfig = false
        }

        dependencies {
            add("coreLibraryDesugaring", libs.findLibrary("android.desugarJdkLibs").get())
        }
    }
}
