// The probe: an instrumentation APK with no dependencies. It runs inside the
// target's process, next to the target's own classes, so it reaches Compose
// by reflection instead of bundling a second (clashing) copy of it.
//
// The target package is a build property; the APK must be signed with the
// same key as the target (here both use the debug key).
plugins {
    id("com.android.application")
}

android {
    namespace = "dev.handsets.probe"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.handsets.probe"
        minSdk = 28
        targetSdk = 36
        manifestPlaceholders["targetPackage"] =
            providers.gradleProperty("target").getOrElse("dev.handsets.sample")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

// AGP 9's built-in Kotlin adds kotlin-stdlib even to Java modules. In the
// target's process the probe's dex comes first on the class path, so a
// bundled stdlib would stand in for the app's own; leave it out of the APK's
// class paths (tools such as lint run on Kotlin themselves).
configurations.matching { it.name.endsWith("RuntimeClasspath") || it.name.endsWith("CompileClasspath") }
    .configureEach { exclude(group = "org.jetbrains.kotlin") }
