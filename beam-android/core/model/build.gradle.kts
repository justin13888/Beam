plugins {
    id("beam.jvm.library")
    alias(libs.plugins.kotlin.serialization)
}

dependencies {
    api(libs.kotlinx.collections.immutable)
    implementation(libs.kotlinx.serialization.json)

    testImplementation(libs.junit)
}
