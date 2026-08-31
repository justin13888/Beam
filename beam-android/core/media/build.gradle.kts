plugins {
    id("beam.android.library")
    id("beam.android.hilt")
    alias(libs.plugins.kotlin.serialization)
}

android {
    namespace = "dev.beam.android.core.media"
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

    testImplementation(projects.core.testing)
    testImplementation(libs.junit)
    testImplementation(libs.robolectric)
    testImplementation(libs.kotlinx.coroutines.test)
}
