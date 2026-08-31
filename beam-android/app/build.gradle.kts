plugins {
    id("beam.android.application")
    id("beam.android.compose")
    id("beam.android.hilt")
    alias(libs.plugins.kotlin.serialization)
}

android {
    namespace = "dev.beam.android"

    defaultConfig {
        applicationId = "dev.beam.android"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }
}

dependencies {
    implementation(projects.core.model)
    implementation(projects.core.ffi)
    implementation(projects.core.designsystem)
    implementation(projects.core.ui)
    implementation(projects.core.media)

    implementation(projects.feature.auth)
    implementation(projects.feature.home)
    implementation(projects.feature.libraries)
    implementation(projects.feature.explore)
    implementation(projects.feature.detail)
    implementation(projects.feature.player)
    implementation(projects.feature.downloads)
    implementation(projects.feature.history)
    implementation(projects.feature.settings)
    implementation(projects.feature.admin)

    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.core.splashscreen)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.navigation3.runtime)
    implementation(libs.androidx.navigation3.ui)
    implementation(libs.androidx.lifecycle.viewmodel.navigation3)
    implementation(libs.androidx.hilt.navigation.compose)
    implementation(libs.androidx.compose.material3.adaptive.navigation.suite)
    implementation(libs.kotlinx.serialization.json)
    // Coil loads artwork through the same authenticated client the API uses,
    // so the app needs to name it. See BeamApplication.
    implementation(libs.coil.compose)
    implementation(libs.coil.network.okhttp)
    implementation(libs.okhttp)
    implementation(libs.androidx.glance.appwidget)
    implementation(libs.androidx.glance.material3)

    testImplementation(projects.core.testing)
}
