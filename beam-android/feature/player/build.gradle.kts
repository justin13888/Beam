plugins {
    id("beam.android.feature")
}

android {
    namespace = "dev.beam.android.feature.player"
}

dependencies {
    // Only the modules that actually play or download depend on Media3. Adding
    // it to the feature convention would put ExoPlayer on the compile
    // classpath of settings and admin, which have no use for it.
    implementation(projects.core.media)
    // The video surface itself. Only the player screen renders one.
    implementation(libs.androidx.media3.ui)
    implementation(libs.androidx.media3.exoplayer)
}
