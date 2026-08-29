plugins {
    id("beam.android.library")
    id("beam.android.compose")
}

android {
    namespace = "dev.beam.android.core.testing"
}

dependencies {
    // Fakes implement the same interfaces production code depends on, so this
    // module needs them on its *main* source set rather than its test one.
    api(projects.core.model)
    api(projects.core.ffi)
    api(projects.core.designsystem)

    api(libs.junit)
    api(libs.robolectric)
    api(libs.roborazzi)
    api(libs.roborazzi.compose)
    api(libs.roborazzi.junit.rule)
    api(libs.turbine)
    api(libs.kotlinx.coroutines.test)
    api(libs.androidx.compose.ui.test.junit4)
    api(libs.androidx.test.ext.junit)
}
