pluginManagement {
    // Convention plugins live in an included build so every module shares one
    // AGP/Kotlin/Compose configuration. Without this the same twenty lines get
    // copied into a dozen build files and drift apart one module at a time.
    includeBuild("build-logic")
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    // Modules may not declare their own repositories: one place decides where
    // artifacts come from, which is the same reasoning ADR-0009 applies to
    // commands.
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        mavenCentral()
    }
}

rootProject.name = "beam-android"

enableFeaturePreview("TYPESAFE_PROJECT_ACCESSORS")

include(":app")

include(":core:model")
include(":core:ffi")
include(":core:designsystem")
include(":core:ui")
include(":core:media")
include(":core:testing")

include(":feature:auth")
include(":feature:home")
include(":feature:libraries")
include(":feature:explore")
include(":feature:detail")
include(":feature:player")
include(":feature:downloads")
include(":feature:history")
include(":feature:settings")
include(":feature:admin")
