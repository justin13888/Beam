plugins {
    id("beam.android.library")
    id("beam.android.compose")
    id("beam.android.screenshot")
}

android {
    namespace = "dev.beam.android.core.designsystem"
}

dependencies {
    api(libs.androidx.compose.material3)
    api(libs.androidx.compose.material3.adaptive)
    api(libs.androidx.compose.material3.adaptive.layout)
    api(libs.androidx.compose.material3.adaptive.navigation)
    api(libs.androidx.compose.material3.adaptive.navigation.suite)
    api(libs.androidx.compose.material3.window.size)

    implementation(libs.coil.compose)
    implementation(libs.coil.network.okhttp)

    // The screenshot dependencies come from `beam.android.screenshot`.
}
