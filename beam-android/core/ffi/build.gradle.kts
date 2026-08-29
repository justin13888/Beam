import com.android.build.api.variant.LibraryAndroidComponentsExtension
import org.gradle.api.DefaultTask
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.tasks.CacheableTask
import org.gradle.api.tasks.InputFile
import org.gradle.api.tasks.Internal
import org.gradle.api.tasks.OutputDirectory
import org.gradle.api.tasks.PathSensitive
import org.gradle.api.tasks.PathSensitivity
import org.gradle.api.tasks.TaskAction
import org.gradle.process.ExecOperations
import javax.inject.Inject

plugins {
    id("beam.android.library")
    id("beam.android.hilt")
}

android {
    namespace = "dev.beam.android.core.ffi"
}

/**
 * The repository root, which is where the mise tasks are defined.
 *
 * ADR-0009 makes mise the single source of truth for every command CI, the
 * git hooks, and a human runs. These tasks therefore shell out to `mise run`
 * rather than restating the cargo invocation, so there is exactly one
 * definition of how the core is built -- while still declaring real inputs and
 * outputs, so Gradle's up-to-date checks and build cache still work.
 */
val repoRoot: java.io.File = rootProject.layout.projectDirectory.asFile.parentFile

@CacheableTask
abstract class MiseTask @Inject constructor(
    private val execOperations: ExecOperations,
) : DefaultTask() {

    @get:Internal
    abstract val workingDirectory: DirectoryProperty

    @get:Input
    abstract val miseTask: Property<String>

    /** The core's sources; a change here invalidates the output. */
    @get:InputDirectory
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val coreSources: DirectoryProperty

    /** The vendored contract the client is generated from. */
    @get:InputFile
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val manifest: RegularFileProperty

    @get:OutputDirectory
    abstract val outputDirectory: DirectoryProperty

    @TaskAction
    fun run() {
        execOperations.exec {
            workingDir = workingDirectory.get().asFile
            commandLine("mise", "run", miseTask.get())
        }
    }
}

val uniffiBindgen = tasks.register<MiseTask>("uniffiBindgen") {
    group = "beam"
    description = "Generate the Kotlin bindings for beam-client-core"
    workingDirectory.set(repoRoot)
    miseTask.set("core:bindgen")
    coreSources.set(File(repoRoot, "beam-client-core/src"))
    manifest.set(File(repoRoot, "beam-client-core/Cargo.toml"))
    outputDirectory.set(layout.buildDirectory.dir("generated/uniffi"))
}

val cargoNdk = tasks.register<MiseTask>("cargoNdk") {
    group = "beam"
    description = "Cross-compile beam-client-core into jniLibs"
    workingDirectory.set(repoRoot)
    miseTask.set("core:android")
    coreSources.set(File(repoRoot, "beam-client-core/src"))
    manifest.set(File(repoRoot, "beam-client-core/Cargo.toml"))
    outputDirectory.set(layout.projectDirectory.dir("src/main/jniLibs"))
}

// Generated sources go through the variant API rather than the source-set DSL:
// AGP refuses a Provider there, and this way the wiring carries the task
// dependency with it instead of relying on a manual dependsOn.
extensions.configure<LibraryAndroidComponentsExtension> {
    onVariants { variant ->
        variant.sources.java?.addGeneratedSourceDirectory(
            uniffiBindgen,
            MiseTask::outputDirectory,
        )
        variant.sources.jniLibs?.addGeneratedSourceDirectory(
            cargoNdk,
            MiseTask::outputDirectory,
        )
    }
}

dependencies {
    api(projects.core.model)

    // UniFFI's Kotlin bindings reach the .so through JNA.
    implementation(variantOf(libs.jna) { artifactType("aar") })
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.androidx.datastore.preferences)

    testImplementation(libs.junit)
    testImplementation(libs.kotlinx.coroutines.test)
}
