/* Fixture (P5, jni): JNI implementations (static registration). */
#include <jni.h>

JNIEXPORT jint JNICALL Java_com_example_Native_compute(JNIEnv *env, jobject self, jint x) {
    return x * 2;
}

JNIEXPORT jint JNICALL Java_com_example_Native_over_1load__I(JNIEnv *env, jobject self, jint x) {
    return x;
}

JNIEXPORT jstring JNICALL Java_com_example_Native_00024Inner_greet(JNIEnv *env, jobject self) {
    return NULL;
}

/* Negative control: no Java class com.example.Other declares a native `compute`. */
JNIEXPORT jint JNICALL Java_com_example_Other_compute(JNIEnv *env, jobject self, jint x) {
    return x;
}
