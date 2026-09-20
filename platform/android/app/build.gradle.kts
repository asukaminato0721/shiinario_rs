plugins { id("com.android.application") }

android {
    namespace = "org.shiinario.player"
    compileSdk = 36
    defaultConfig {
        applicationId = "org.shiinario.player"
        minSdk = 28
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
    }
    buildTypes {
        release { isMinifyEnabled = false }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
