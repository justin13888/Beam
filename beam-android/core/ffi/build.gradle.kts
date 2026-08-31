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
import org.gradle.api.tasks.TaskProvider
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

    /**
     * Environment for the task, which is how the cargo profile is selected.
     *
     * An `@Input` rather than `@Internal`: the profile changes the bytes of
     * the output, so a build that switches profile must not be considered
     * up to date against the other profile's artifacts.
     */
    @get:Input
    abstract val environment: MapProperty<String, String>

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
            environment(this@MiseTask.environment.get())
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

/**
 * Cross-compile the core, at the profile the variant needs.
 *
 * Registered per variant rather than once, which is load-bearing: a single
 * task would build one profile for both, and `./gradlew assembleRelease` would
 * package the debug library -- an unstripped ~83 MB `.so` per ABI, and only
 * the emulator's x86_64, so the APK would be an order of magnitude too large
 * and would crash on an actual phone. Only the `android:release` mise task set
 * the profile before, so the trap was live for anyone invoking Gradle
 * directly.
 *
 * The output directory is per profile too, since cargo-ndk overwrites only the
 * ABIs it builds and would otherwise leave the other profile's libraries in
 * place to be packaged.
 */
fun registerCargoNdk(variantName: String): TaskProvider<MiseTask> {
    val isRelease = variantName.equals("release", ignoreCase = true)
    val profile = if (isRelease) "release" else "debug"

    return tasks.register<MiseTask>("cargoNdk${variantName.replaceFirstChar(Char::uppercase)}") {
        group = "beam"
        description = "Cross-compile beam-client-core into jniLibs ($profile)"
        workingDirectory.set(repoRoot)
        miseTask.set("core:android")
        coreSources.set(File(repoRoot, "beam-client-core/src"))
        manifest.set(File(repoRoot, "beam-client-core/Cargo.toml"))
        environment.set(
            mapOf(
                "BEAM_CARGO_PROFILE" to profile,
                "BEAM_JNI_LIBS_DIR" to
                    layout.buildDirectory.dir("jniLibs/$profile").get().asFile.absolutePath,
            ),
        )
        outputDirectory.set(layout.buildDirectory.dir("jniLibs/$profile"))
    }
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
            registerCargoNdk(variant.name),
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
