// Rule fixture (chained calls share a callee start): the program is an argument of the
// inner call.
package procs

import "os/exec"

func Build() error {
	return exec.Command("python", "procs/build.py").Run()
}
