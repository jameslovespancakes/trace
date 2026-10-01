// Fixture (P5, grpc): Go server implementing helloworld.Greeter.
package main

import (
	"context"

	pb "example.com/helloworld"
	"google.golang.org/grpc"
)

type server struct {
	pb.UnimplementedGreeterServer
}

func (s *server) SayHello(ctx context.Context, in *pb.HelloRequest) (*pb.HelloReply, error) {
	return &pb.HelloReply{Message: "Hello " + in.GetName()}, nil
}

func (s *server) SayGoodbye(ctx context.Context, in *pb.HelloRequest) (*pb.HelloReply, error) {
	return &pb.HelloReply{Message: "Bye"}, nil
}

func main() {
	s := grpc.NewServer()
	pb.RegisterGreeterServer(s, &server{})
}
