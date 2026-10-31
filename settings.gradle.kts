pluginManagement {
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
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
        maven {
            url = uri("https://oss.sonatype.org/content/repositories/snapshots")
        }
    }
}


plugins {
    id("org.gradle.toolchains.foojay-resolver-convention") version "1.0.0"
    id("com.android.application") version "9.5.0" apply false
    // AGP 9 compiles Kotlin itself; this only pins the Kotlin compiler version.
    kotlin("android") version "2.4.21" apply false
    kotlin("plugin.compose") version "2.4.21" apply false
}

rootProject.name = "handsets-daemon"

// Prototype (probe/): an instrumentation probe that reads a Compose app's
// semantics tree from inside it, and a small Compose app to point it at.
//   ./gradlew :probe:assembleRelease :probe-sample:assembleRelease -Ptarget=dev.handsets.sample
include(":probe", ":probe-sample")
project(":probe").projectDir = file("probe/probe")
project(":probe-sample").projectDir = file("probe/sample")
