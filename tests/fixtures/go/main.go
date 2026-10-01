package main

import "fmt"

type Server struct {
	name string
}

func (s *Server) Run() string {
	return greet(s.name)
}

func greet(name string) string {
	return fmt.Sprintf("hello %s", name)
}

func main() {
	s := &Server{name: "go"}
	fmt.Println(s.Run())
}
