/**
 * JPEG Encoder - JNI Native Implementation
 *
 * Hardware-accelerated JPEG encoding using libjpeg-turbo.
 * Called from ScreenshotPipeline.kt for fast screenshot compression.
 *
 * Performance target: <20ms for 1080p JPEG encoding with quality 80
 *
 * Build requirements:
 * - libjpeg-turbo library
 * - Android NDK with NEON support
 */

#include <jni.h>
#include <android/bitmap.h>
#include <android/log.h>
#include <cstring>
#include <vector>

// #include <turbojpeg.h>  // Enable after libjpeg-turbo integration

#define LOG_TAG "Magdroid-JNI"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, LOG_TAG, __VA_ARGS__)
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, LOG_TAG, __VA_ARGS__)
#define LOGD(...) __android_log_print(ANDROID_LOG_DEBUG, LOG_TAG, __VA_ARGS__)

/**
 * Encode Android Bitmap to JPEG using libjpeg-turbo
 *
 * @param env JNI environment
 * @param thiz Object instance (ScreenshotPipeline)
 * @param bitmap Source bitmap (ARGB_8888)
 * @param quality JPEG quality (1-100)
 * @return JPEG bytes as jbyteArray
 */
extern "C" JNIEXPORT jbyteArray JNICALL
Java_com_magdroid_companion_screenshot_ScreenshotPipeline_encodeJpegNative(
    JNIEnv* env,
    jobject /* thiz */,
    jobject bitmap,
    jint quality) {

    LOGI("JPEG encoding requested: quality=%d", quality);

    // Get bitmap info
    AndroidBitmapInfo info;
    int result = AndroidBitmap_getInfo(env, bitmap, &info);
    if (result != ANDROID_BITMAP_RESULT_SUCCESS) {
        LOGE("Failed to get bitmap info: %d", result);
        return nullptr;
    }

    LOGD("Bitmap info: %dx%d, format=%d", info.width, info.height, info.format);

    // Validate format
    if (info.format != ANDROID_BITMAP_FORMAT_RGBA_8888) {
        LOGE("Unsupported bitmap format: %d (expected RGBA_8888)", info.format);
        return nullptr;
    }

    // Lock bitmap pixels
    void* pixels;
    result = AndroidBitmap_lockPixels(env, bitmap, &pixels);
    if (result != ANDROID_BITMAP_RESULT_SUCCESS) {
        LOGE("Failed to lock bitmap pixels: %d", result);
        return nullptr;
    }

    // Stub: libjpeg-turbo not yet integrated — returns empty array
    LOGE("Native JPEG encoding not available, using Java fallback");

    // Unlock bitmap
    AndroidBitmap_unlockPixels(env, bitmap);

    // Return empty array
    jbyteArray empty_array = env->NewByteArray(0);
    return empty_array;
}

/**
 * Get library version
 *
 * Utility function to verify JNI is working correctly.
 */
extern "C" JNIEXPORT jstring JNICALL
Java_com_magdroid_companion_screenshot_ScreenshotPipeline_getNativeLibraryVersion(
    JNIEnv* env,
    jobject /* thiz */) {

    return env->NewStringUTF("Magdroid JNI v0.1.0");
}

/**
 * JNI_OnLoad - Called when library is loaded
 */
JNIEXPORT jint JNI_OnLoad(JavaVM* /* vm */, void* /* reserved */) {
    LOGI("Magdroid JNI library loaded");
    return JNI_VERSION_1_6;
}

/**
 * JNI_OnUnload - Called when library is unloaded
 */
JNIEXPORT void JNI_OnUnload(JavaVM* /* vm */, void* /* reserved */) {
    LOGI("Magdroid JNI library unloaded");
}
