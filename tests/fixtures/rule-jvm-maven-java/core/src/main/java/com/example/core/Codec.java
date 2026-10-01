package com.example.core;

import com.google.gson.Gson;

public final class Codec {
    private final Gson gson = new Gson();

    public String encode(Object value) {
        return gson.toJson(value);
    }
}
