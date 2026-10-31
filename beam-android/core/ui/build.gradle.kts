plugins {
    id("beam.android.library")
    id("beam.android.compose")
    // `Format.resolution` is the core's rule.
    id("beam.android.nativecore")
}

android {
    namespace = "dev.beam.android.core.ui"
}

dependencies {
    api(projects.core.designsystem)
    api(projects.core.model)
    // The core's records are the vocabulary these components render, so they
    // are part of this module's API rather than an implementation detail.
    api(projects.core.ffi)

    testImplementation(libs.junit)
}
