// Rule fixture (procffi derivation rules): a field a constructor fills and a method sends.
package procs

import "proc"

type Cmd struct {
	Path string
	Dir  string
}

func Command(name string, dir string) *Cmd {
	cmd := &Cmd{Path: name}
	cmd.Dir = dir
	return cmd
}

func (c *Cmd) Start() error {
	lp := c.Path
	return proc.Spawn(lp, c.Dir)
}
