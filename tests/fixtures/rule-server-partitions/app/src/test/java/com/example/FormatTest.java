package com.example;

import com.example.Helper;

public class FormatTest {
    static String banner(String name) {
        return Helper.pad(name);
    }

    void checksBanner() {
        String s = banner("x");
        if (!s.startsWith("[")) {
            throw new AssertionError(s);
        }
    }
}
