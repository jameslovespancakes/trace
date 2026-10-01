// Fixture (P5, grpc): a second implementation of SayGoodbye (makes it ambiguous).
package main

import (
	"context"

	pb "example.com/helloworld"
)

type legacy struct {
	pb.UnimplementedGreeterServer
}

func (l *legacy) SayGoodbye(ctx context.Context, in *pb.HelloRequest) (*pb.HelloReply, error) {
	return &pb.HelloReply{Message: "Farewell"}, nil
}
