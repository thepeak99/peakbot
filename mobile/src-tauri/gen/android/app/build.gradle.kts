import java.io.FileInputStream
import java.util.Properties
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

android {
    compileSdk = 37
    namespace = "sh.shifu.mobile"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "sh.shifu.mobile"
        minSdk = 24
        targetSdk = 37
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
    }
    // Release signing: keystore.properties carries storePassword,
    // keyPassword, keyAlias and storeFile. Path: the ANDROID_KEYSTORE_PROPERTIES
    // env var (CI: the tmpfs file decoded from the BuildKit secrets), else
    // the Tauri convention rootProject/keystore.properties
    // (git-ignored). No file ⇒ no release signingConfig ⇒ a release build
    // refuses to assemble; debug builds are unaffected (they use AGP's
    // default debug keystore from /root/.android).
    signingConfigs {
        val keystorePropertiesFile = file(
            System.getenv("ANDROID_KEYSTORE_PROPERTIES")
                ?: rootProject.file("keystore.properties").absolutePath
        )
        if (keystorePropertiesFile.exists()) {
            val keystoreProperties = Properties().apply {
                FileInputStream(keystorePropertiesFile).use { load(it) }
            }
            create("release") {
                keyAlias = keystoreProperties.getProperty("keyAlias")
                keyPassword = keystoreProperties.getProperty("keyPassword")
                storeFile = file(keystoreProperties.getProperty("storeFile") ?: "")
                storePassword = keystoreProperties.getProperty("storePassword")
            }
        }
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {
                // No armv7/i686: release is arm64-v8a only; x86_64 is for dev
                // emulator APKs.
                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            // Sign with the release key when keystore.properties is
            // present (tmpfs in CI); without it a release build fails
            // loudly at the signing step.
            signingConfigs.findByName("release")?.let { signingConfig = it }
            optimization {
               enable = true
            }
            proguardFiles(
                *fileTree(".") {
                  include("**/*.pro")
                  exclude("build/**")
                }.files.toTypedArray()
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    buildFeatures {
        buildConfig = true
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_1_8
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = file("tauri.build.gradle.kts"))
