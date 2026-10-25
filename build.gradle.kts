import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import java.io.FileReader
import java.util.Properties

/*
 * Build for the on-device daemon in src/.
 *
 * Not an Android application: Main is launched by app_process on a device.
 * Pipeline: javac + kotlinc (against android.jar) -> R8 (from the SDK's
 * build-tools d8.jar) -> classes.dex -> build/hs.jar.
 *
 *   ./gradlew hsJar                       build build/hs.jar
 *   -PandroidCompileSdk=36                pick a platform (default: newest installed)
 *   -PbuildTools=36.0.0                   pick build-tools for R8 (default: 36.0.0)
 */
plugins {
    `java-library`
    kotlin("jvm")
}

val localProperties = Properties().apply {
    val f = rootProject.file("local.properties")
    if (f.isFile) FileReader(f).use { load(it) }
}

val sdkDirectory: String = localProperties.getProperty("sdk.dir")
    ?: System.getenv("ANDROID_HOME")
    ?: System.getenv("ANDROID_SDK_ROOT")
    ?: throw GradleException(
        "Android SDK not found. Set sdk.dir in local.properties or ANDROID_HOME / ANDROID_SDK_ROOT.")

val compileSdk: String = providers.gradleProperty("androidCompileSdk").orNull
    ?: file("$sdkDirectory/platforms").listFiles()
        ?.mapNotNull { Regex("""android-(\d+)""").matchEntire(it.name)?.groupValues?.get(1)?.toInt() }
        ?.maxOrNull()?.toString()
    ?: throw GradleException("No android-N platform installed under $sdkDirectory/platforms")

val androidJar = file("$sdkDirectory/platforms/android-$compileSdk/android.jar")
if (!androidJar.isFile) {
    throw GradleException("Android platform jar not found: $androidJar. " +
        "Install that platform or set -PandroidCompileSdk=<API level>.")
}

val buildToolsVersion: String = providers.gradleProperty("buildTools").orNull ?: "36.0.0"
val r8Jar = file("$sdkDirectory/build-tools/$buildToolsVersion/lib/d8.jar")

// Gradle's own outputs stay out of build/, which holds only the deployable jar.
layout.buildDirectory = layout.projectDirectory.dir("build-gradle")

sourceSets {
    main {
        java.setSrcDirs(listOf("src"))
        resources.setSrcDirs(emptyList<String>())
    }
}

kotlin {
    sourceSets {
        main { kotlin.setSrcDirs(listOf("src")) }
    }
    compilerOptions {
        jvmTarget = JvmTarget.JVM_1_8
        // java.* comes from android.jar, not the host JDK.
        freeCompilerArgs.add("-no-jdk")
    }
}

java {
    // -bootclasspath is only accepted with target 8.
    sourceCompatibility = JavaVersion.VERSION_1_8
    targetCompatibility = JavaVersion.VERSION_1_8
}

dependencies {
    compileOnly(files(androidJar))
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
}

tasks.withType<JavaCompile>().configureEach {
    options.encoding = "UTF-8"
    options.bootstrapClasspath = files(androidJar)
    options.compilerArgs.add("-Xlint:-options")
}

val r8Out = layout.buildDirectory.dir("r8")

val r8Dex by tasks.registering(JavaExec::class) {
    description = "Shrinks and dexes the daemon plus its runtime libraries with the SDK's R8."
    val programJars = files(tasks.named("jar"), configurations.runtimeClasspath)
    inputs.files(programJars)
    inputs.file("proguard-rules.pro")
    outputs.dir(r8Out)
    classpath = files(r8Jar)
    mainClass = "com.android.tools.r8.R8"
    doFirst {
        if (!r8Jar.isFile) throw GradleException("R8 not found: $r8Jar (set -PbuildTools=<version>)")
        val out = r8Out.get().asFile
        out.deleteRecursively()
        out.mkdirs()
        args = listOf(
            "--release",
            "--min-api", "28",
            "--lib", androidJar.path,
            "--pg-conf", file("proguard-rules.pro").path,
            "--output", out.path,
        ) + programJars.files.map { it.path }
    }
}

val hsJar by tasks.registering(Jar::class) {
    group = "build"
    description = "Builds build/hs.jar (classes.dex only) for app_process."
    from(r8Dex) { include("classes.dex") }
    // ServiceLoader entry so Dispatchers.Main finds AndroidDispatcherFactory at runtime.
    from({ configurations.runtimeClasspath.get().filter { it.name.startsWith("kotlinx-coroutines-android") }.map { zipTree(it) } }) {
        include("META-INF/services/kotlinx.coroutines.internal.MainDispatcherFactory")
    }
    archiveFileName = "hs.jar"
    destinationDirectory = layout.projectDirectory.dir("build")
    isPreserveFileTimestamps = false
    manifest { }
}

tasks.named("assemble") { dependsOn(hsJar) }
