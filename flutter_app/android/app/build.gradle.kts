import java.util.Properties

plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

// ---- Release signing (CR-P2-02) ------------------------------------------
// The release APK is signed with the owner's key, read from an UNTRACKED
// key.properties file at flutter_app/android/key.properties. Expected fields:
//
//     storeFile=<path to the keystore, absolute or relative to android/app/>
//     storePassword=<keystore password>
//     keyAlias=<key alias inside the keystore>
//     keyPassword=<password of that key>
//
// key.properties and every keystore format (*.keystore, *.jks,
// upload-keystore*) are gitignored in flutter_app/.gitignore; this build file
// contains no key material and no real paths.
//
// There is deliberately NO fallback to the debug keystore: a release APK
// silently signed with debug keys cannot be distributed or upgraded in place,
// so a missing or incomplete key.properties must fail the release build
// loudly (GradleException below) instead of producing a broken artifact.
// Debug and profile builds are unaffected: they never use this signing
// config and keep signing with the debug keystore as before.
val keystorePropertiesFile = rootProject.file("key.properties")
val keystoreProperties = Properties().apply {
    if (keystorePropertiesFile.exists()) {
        keystorePropertiesFile.inputStream().use { load(it) }
    }
}
val requiredSigningFields = listOf("storeFile", "storePassword", "keyAlias", "keyPassword")
val releaseSigningComplete =
    keystorePropertiesFile.exists() &&
        requiredSigningFields.none { keystoreProperties.getProperty(it).isNullOrBlank() }

fun assertReleaseSigningConfigured() {
    if (!keystorePropertiesFile.exists()) {
        throw GradleException(
            "Release signing is not configured: flutter_app/android/key.properties was not found.\n" +
                "Create it with the four fields storeFile, storePassword, keyAlias and keyPassword\n" +
                "(format documented in the comment atop app/build.gradle.kts). The file and the\n" +
                "keystore it points to must stay untracked (they are gitignored). Release builds\n" +
                "have NO debug-signing fallback by design; debug/profile builds do not need this file.",
        )
    }
    val missingFields = requiredSigningFields.filter { keystoreProperties.getProperty(it).isNullOrBlank() }
    if (missingFields.isNotEmpty()) {
        throw GradleException(
            "Release signing is not configured: key.properties has missing or empty field(s): $missingFields.\n" +
                "Fill in storeFile, storePassword, keyAlias and keyPassword in flutter_app/android/key.properties.",
        )
    }
}

// Fail fast — at configuration time, before any compile/packaging work — but
// ONLY when a release task was requested: `flutter build apk --release` runs
// `gradlew assembleRelease`, appbundle -> bundleRelease. Without this guard a
// missing key.properties would also break debug builds (configuration of the
// release signing config happens for every invocation).
if (gradle.startParameter.taskNames.any { it.substringAfterLast(':').endsWith("Release", ignoreCase = true) }) {
    assertReleaseSigningConfigured()
}

android {
    namespace = "dev.ak.ak_sending_app"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    defaultConfig {
        applicationId = "dev.ak.ak_sending_app"
        // Pinned to 24 (== flutter.minSdkVersion): the Rust ak-ffi cdylib is
        // cross-compiled with the NDK clang android24 triples, so the store
        // must not advertise anything older.
        minSdk = 24
        // Pinned to 36 == the CURRENT value of flutter.targetSdkVersion in the
        // installed Flutter (FlutterExtension.kt), instead of floating with it.
        // Floating silently crosses API 37 (Android 17), where targetSdk >= 37
        // requires the runtime ACCESS_LOCAL_NETWORK permission (NEARBY_DEVICES
        // group) for ANY LAN access (TCP/UDP/mDNS) — the app's local-direct
        // path would degrade to timeouts/EPERM without a single code change.
        // Raising this requires the permission flow registered in
        // docs/BACKLOG.zh-CN.md (CR-P2-03).
        targetSdk = 36
        // Uses the version code from pubspec.yaml. When using split APKs, 1000 * ABI_VERSION
        // is added automatically by Flutter. (https://developer.android.com/studio/build/configure-apk-splits#configure-APK-versions)
        // You can force using the value of versionCode by specifying the `-P force-version-code-ignoring-abi=true`
        // flag during build.
        versionCode = flutter.versionCode
        versionName = flutter.versionName
        // Only the ABIs scripts/build_android_ffi.sh cross-compiles
        // libak_ffi.so for (arm64 devices + x86_64 emulators). Without
        // this filter the release APK also packaged Flutter's own
        // armeabi-v7a libs WITHOUT libak_ffi.so — a 32-bit ARM phone
        // would install that variant and die at System.loadLibrary.
        ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
        // Hard strip: some AAR-native libs (e.g. datastore's shared-counter)
        // slip past ndk.abiFilters and re-introduce an armeabi-v7a lib/ dir
        // WITHOUT libak_ffi.so — a 32-bit phone would pick that ABI and die
        // at System.loadLibrary. packaging.jniLibs.excludes removes every
        // non-supported ABI dir outright.
        packaging {
            jniLibs {
                excludes += listOf("lib/armeabi-v7a/**", "lib/x86/**")
            }
        }
    }

    signingConfigs {
        create("release") {
            // Values are assigned only when key.properties is complete; the
            // check above has already failed release builds when it is not,
            // so an empty config can only be observed by non-release builds,
            // which never consume it.
            if (releaseSigningComplete) {
                storeFile = file(keystoreProperties.getProperty("storeFile"))
                storePassword = keystoreProperties.getProperty("storePassword")
                keyAlias = keystoreProperties.getProperty("keyAlias")
                keyPassword = keystoreProperties.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        release {
            signingConfig = signingConfigs.getByName("release")
        }
    }
}

// Safety net for release tasks that enter the graph WITHOUT being named on
// the command line (e.g. a bare `gradlew build`): fail before packaging
// rather than crashing inside AGP with a null signing config. The flutter
// tool path is already covered by the check above the android {} block.
tasks.matching { it.name == "packageRelease" || it.name == "bundleRelease" }
    .configureEach { doFirst { assertReleaseSigningConfigured() } }

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}
