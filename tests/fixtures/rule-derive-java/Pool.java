package com.acme;

import java.util.ArrayList;
import java.util.List;
import java.util.function.Function;

/** Functional parameters (rules: the single method of a functional interface calls the passed
 * function; stored in a field and run later is stored_then_called; data is never run). */
public class Pool {
    private Runnable onClose;
    private final List<Runnable> tasks = new ArrayList<>();

    public Pool(Runnable onClose) {
        this.onClose = onClose;
    }

    public void submit(Runnable task) {
        task.run();
    }

    public <T, R> R apply(Function<T, R> fn, T value) {
        return fn.apply(value);
    }

    public void later(Runnable task) {
        tasks.add(task);
    }

    public void close() {
        onClose.run();
        for (Runnable t : tasks) {
            t.run();
        }
    }

    public void register(Listener listener) {
        listener.onEvent("closed");
    }

    public String name(Object value) {
        return value.toString();
    }
}

interface Listener {
    void onEvent(String name);
}
