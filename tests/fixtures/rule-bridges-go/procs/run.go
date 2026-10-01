// Fixture (bridges gate, go subprocess family).
package procs

import "os/exec"

func Build() error {
	return exec.Command("python", "procs/build.py").Run()
}
