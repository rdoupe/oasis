plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

// android/build.sh puts the native libraries (the bridge and OpenSSH) and fonts here.
val generated = layout.buildDirectory.dir("generated/oasis")

android {
    namespace = "app.oasis.mobile"
    compileSdk = 37

    defaultConfig {
        applicationId = "app.oasis.mobile"
        minSdk = 29
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    packaging {
        // ssh runs from the installed library directory, so it must be extracted.
        jniLibs.useLegacyPackaging = true
    }

    sourceSets {
        getByName("main") {
            jniLibs.directories.add(generated.get().dir("jniLibs").asFile.path)
            res.directories.add(generated.get().dir("res").asFile.path)
        }
    }
}

dependencies {
    val bom = platform("androidx.compose:compose-bom:2026.09.00")
    implementation(bom)
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.compose.ui:ui")
}
