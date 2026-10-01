package com.example.app;

import com.example.core.Codec;
import java.util.List;

public final class Main {
    public static void main(String[] args) {
        Codec codec = new Codec();
        List.of("a", "b").forEach(s -> System.out.println(codec.encode(s)));
    }
}
