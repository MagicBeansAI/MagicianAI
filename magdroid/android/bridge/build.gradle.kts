plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.serialization")
}

android {
    namespace = "ai.magicbeans.magdroid.bridge"
    compileSdk = 34

    defaultConfig {
        minSdk = 24
        consumerProguardFiles("consumer-rules.pro")
        externalNativeBuild { cmake { cppFlags += "-std=c++17" } }
    }

    externalNativeBuild {
        cmake {
            path = file("src/main/cpp/CMakeLists.txt")
            version = "3.22.1"
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }

    sourceSets["main"].java.srcDirs("src/main/kotlin")
    sourceSets["test"].java.srcDirs("src/test/kotlin")
}

// The built-in tutor primitives are the backend's set, not a copy of it.
//
// They are copied into assets at build time from the canonical folder the
// server itself seeds and serves. iOS hand-maintains its bundled copy, which
// means a primitive added to the backend reaches iOS when somebody remembers;
// here it arrives on the next build. The bundle only matters before the first
// successful fetch — offline, or a first launch — after which the registry
// draws from what the backend served.
val canonicalTutorPrimitives = rootProject.file("../../magician_data_v3/system/tutor_primitives")
val generatedPrimitiveAssets = layout.buildDirectory.dir("generated/tutorPrimitives")

val syncTutorPrimitives = tasks.register<Sync>("syncTutorPrimitives") {
    description = "Copies the canonical tutor primitive recipes into bundled assets."
    from(canonicalTutorPrimitives) { include("*.json") }
    into(generatedPrimitiveAssets.map { it.dir("tutor_primitives") })
    // A checkout without the data folder still builds. The app then draws only
    // what the backend serves, which is the normal path anyway.
    onlyIf { canonicalTutorPrimitives.isDirectory }
}

android {
    sourceSets.getByName("main").assets.srcDir(generatedPrimitiveAssets)
}

// The asset merger is the actual consumer, so the dependency belongs there
// rather than on `preBuild` — assets copied after the merge ran would ship an
// empty folder.
tasks.withType<com.android.build.gradle.tasks.MergeSourceSetFolders>().configureEach {
    dependsOn(syncTutorPrimitives)
}

// The verification-code extractor is checked against the runtime's own
// fixture list (secure HITL P6). Declaring the file as a test input makes an
// edit to the fixtures re-run the unit tests instead of reporting the last
// result as up to date.
tasks.withType<Test>().configureEach {
    inputs.file(rootProject.file("../../magician/src/magician_v2/verification_codes/extraction_fixtures.json"))
        .withPathSensitivity(PathSensitivity.RELATIVE)
}

dependencies {
    implementation("org.jetbrains.kotlin:kotlin-stdlib:2.0.21")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
    implementation("androidx.core:core-ktx:1.12.0")
    implementation("androidx.appcompat:appcompat:1.6.1")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.7.3")

    val ktorVersion = "3.0.3"
    // The companion only dials out. MCP runs over this authenticated WebSocket;
    // no HTTP server or listening port is packaged into the APK.
    implementation("io.ktor:ktor-client-core:$ktorVersion")
    implementation("io.ktor:ktor-client-cio:$ktorVersion")
    implementation("io.ktor:ktor-client-websockets:$ktorVersion")

    implementation("androidx.work:work-runtime-ktx:2.9.1")
    // The chat ViewModel lives here with the rest of the connection layer, so
    // the lifecycle artifacts belong to this module rather than the shell.
    implementation("androidx.lifecycle:lifecycle-viewmodel-ktx:2.8.6")
    implementation("androidx.security:security-crypto:1.1.0-alpha06")
    // Apps physical-owner authority accepts only server-decoded standard
    // Play Integrity verdicts bound to the exact enrollment/socket proof.
    implementation("com.google.android.play:integrity:1.6.0")

    testImplementation("junit:junit:4.13.2")
    // The frame pusher's whole behaviour is what happens while an upload is in
    // flight, which needs a scheduler that can be held still.
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.9.0")
    testImplementation("io.ktor:ktor-client-mock:$ktorVersion")
    // On-device wake word. The same Vosk engine and the same model the iOS app
    // and the desktop tray already use, so a phrase that works on one works on
    // all three.
    implementation("com.alphacephei:vosk-android:0.3.47@aar")
    implementation("net.java.dev.jna:jna:5.13.0@aar")

}
