// Fixture (bridges gate, java subprocess family).
package demo;

public class Procs {
    public Process build() throws Exception {
        return new ProcessBuilder("python", "procs/build.py").start();
    }
}
