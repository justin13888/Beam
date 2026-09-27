plugins {
    id("beam.android.library")
    id("beam.android.hilt")
    alias(libs.plugins.kotlin.serialization)
}

android {
    namespace = "dev.beam.android.core.media"
}

// The download tests run under Robolectric, and the API level it can run is
// pinned once for every module in this shared file. Configured through the
// typed DSL because the generated `android` accessor still names AGP's removed
// source-set type.
configure<com.android.build.api.dsl.LibraryExtension> {
    sourceSets.getByName("test").resources.srcDir(
        rootProject.layout.projectDirectory.dir("gradle/robolectric"),
    )
}

dependencies {
    api(projects.core.model)
    implementation(projects.core.ffi)

    api(libs.androidx.media3.exoplayer)
    api(libs.androidx.media3.session)
    api(libs.androidx.media3.common)
    // The same OkHttp client carries the session cookie and the trust
    // decision the core resolved, so playback and the API agree about who
    // the user is and which certificate is acceptable.
    implementation(libs.androidx.media3.datasource.okhttp)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.okhttp)
    implementation(libs.kotlinx.coroutines.android)
    // A download's poster is kept in the app's own image loader, so the
    // downloads screen renders it offline from the entry it would read anyway.
    implementation(libs.coil.singleton)

    testImplementation(projects.core.testing)
    testImplementation(libs.junit)
    testImplementation(libs.robolectric)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation(libs.coil.network.okhttp)
}
