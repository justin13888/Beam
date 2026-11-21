plugins {
    id("beam.android.feature")
    id("beam.android.pitest")
}

android {
    namespace = "dev.beam.android.feature.settings"
}

dependencies {
    // Forgetting a server deletes the downloads fetched from it, and the
    // downloads live in the Media3 module.
    implementation(projects.core.media)
}
