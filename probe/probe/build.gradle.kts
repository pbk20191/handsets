// The probe: an instrumentation APK. It runs inside the target's process,
// next to the target's own classes, so it reaches Compose by reflection
// instead of bundling a second (clashing) copy of it.
//
// Its libraries, androidx.test:monitor (MonitoringInstrumentation) and what
// it needs (kotlin-stdlib, androidx.tracing), are R8-repackaged under
// dev.handsets.probe.lib: the probe's dex comes first on the class path, and
// a bundled class under its own name would stand in for the app's copy. Nor
// can the probe lean on the app's copy: in a minified app it is renamed.
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

    buildTypes {
        // A release build: R8 doesn't rename anything in a debuggable one.
        // Signed with the debug key, the target's key in this prototype.
        release {
            isMinifyEnabled = true
            signingConfig = signingConfigs.getByName("debug")
            proguardFiles("probe-rules.pro")
            // monitor ships `-keepnames class androidx.test.**`, which would
            // keep its classes under their own names: the clash this avoids.
            optimization { keepRules { ignoreExternalDependencies("androidx.test:monitor") } }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation("androidx.test:monitor:1.7.2")
}
