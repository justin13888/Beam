plugins {
    id("beam.android.feature")
    id("beam.android.pitest")
}

android {
    namespace = "dev.beam.android.feature.admin"
}

dependencies {
    // The admin area's pages are a back stack of their own, each page's view
    // model scoped to its entry, as the app's top-level destinations are.
    implementation(libs.androidx.lifecycle.viewmodel.navigation3)
}
