// The target app. Nothing in it knows about the probe.
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "dev.handsets.sample"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.handsets.sample"
        minSdk = 28
        targetSdk = 36
    }

    buildFeatures {
        compose = true
    }

    buildTypes {
        // Shipped the way real apps are: R8-minified, no keep rules for the
        // probe. Signed with the debug key so the probe can instrument it.
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2025.06.00"))
    implementation("androidx.compose.material3:material3")
    implementation("androidx.activity:activity-compose:1.10.1")
}
