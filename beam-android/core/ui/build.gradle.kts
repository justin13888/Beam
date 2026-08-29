plugins {
    id("beam.android.library")
    id("beam.android.compose")
}

android {
    namespace = "dev.beam.android.core.ui"
}

dependencies {
    api(projects.core.designsystem)
    api(projects.core.model)

    implementation(libs.androidx.paging.compose)
    implementation(libs.coil.compose)

    testImplementation(libs.junit)
}
