// Prototype: an instrumentation "probe" that runs inside an app it doesn't
// modify, and reads that app's Compose semantics tree.
//
//   :sample  a small Compose app — the target, built once and left alone
//   :probe   the instrumentation APK; signed with the same (debug) key
//
//   ../gradlew -p probe assembleDebug -Ptarget=dev.handsets.sample
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

plugins {
    id("org.gradle.toolchains.foojay-resolver-convention") version "1.0.0"
    id("com.android.application") version "9.4.1" apply false
    // AGP 9 compiles Kotlin itself; these pin the compiler and its Compose plugin.
    id("org.jetbrains.kotlin.android") version "2.2.21" apply false
    id("org.jetbrains.kotlin.plugin.compose") version "2.2.21" apply false
}

rootProject.name = "handsets-probe"
include(":sample", ":probe")
