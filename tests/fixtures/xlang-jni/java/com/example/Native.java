// Fixture (P5, jni): Java native methods implemented in C.
package com.example;

public class Native {
    /** Unique: Java_com_example_Native_compute in c/native.c -> proven. */
    public native int compute(int x);

    /** Unique through the overload-qualified C name (Java_..._over_1load__I). */
    public native int over_load(int x);

    /** Negative control: not native. */
    public int notNative() {
        return 0;
    }

    static class Inner {
        /** Ambiguous: defined in c/native.c and c/native2.c -> possible. */
        native String greet();
    }
}
