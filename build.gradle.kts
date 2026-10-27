/*
 * Build for the on-device daemon in src/.
 *
 * Not a launchable app: Main is started by app_process with the APK on
 * CLASSPATH (app_process only needs classes.dex inside a zip). AGP handles
 * the SDK (platform + build-tools, downloaded if missing), Kotlin, and R8;
 * hsJar copies the release APK to build/hs.jar, the path the CLI, installer
 * and release workflow expect.
 *
 *   ./gradlew hsJar                       build build/hs.jar
 */
plugins {
    id("com.android.application")
}

// Gradle's own outputs stay out of build/, which holds only the deployable jar.
layout.buildDirectory = layout.projectDirectory.dir("build-gradle")

android {
    namespace = "dev.handsets.daemon"
    compileSdk = 36

    defaultConfig {
        minSdk = 28
        targetSdk = 36
    }

    sourceSets {
        named("main") {
            manifest.srcFile("AndroidManifest.xml")
            java.setSrcDirs(listOf("src"))
            kotlin.setSrcDirs(listOf("src"))
            res.setSrcDirs(emptyList<String>())
            resources.setSrcDirs(emptyList<String>())
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles("proguard-rules.pro")
            // app_process doesn't check signatures; leave the APK unsigned.
            signingConfig = null
            vcsInfo.include = false
        }
    }

    packaging {
        // Compress classes.dex: hs.jar is pushed over adb and shipped in release
        // tarballs, and nothing gains from mapping it uncompressed.
        dex.useLegacyPackaging = true
        resources {
            // Kotlin reflection/debug metadata the daemon never reads. Keep
            // META-INF/services: Dispatchers.Main is found through it.
            excludes += listOf("kotlin/**", "DebugProbesKt.bin", "META-INF/*.version")
        }
    }

    dependenciesInfo {
        includeInApk = false
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    lint {
        checkReleaseBuilds = false
    }
}

kotlin {
    compilerOptions {
        // Drop Intrinsics.checkNotNull* calls: R8 can't remove them, and the
        // daemon has no Java callers that could pass unexpected nulls.
        freeCompilerArgs.addAll("-Xno-param-assertions", "-Xno-call-assertions", "-Xno-receiver-assertions")
    }
}

dependencies {
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
}

val hsJar = tasks.register<Copy>("hsJar") {
    group = "build"
    description = "Builds build/hs.jar (the release APK) for app_process."
    from(tasks.named("assembleRelease").map { layout.buildDirectory.dir("outputs/apk/release") }) {
        include("*.apk")
        rename { "hs.jar" }
    }
    into(layout.projectDirectory.dir("build"))
}

tasks.named("assemble") { dependsOn(hsJar) }
