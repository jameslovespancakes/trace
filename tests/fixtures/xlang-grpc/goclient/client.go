// Fixture (round 2, grpc): Go client calling through a stub created inline by the generated
// factory `NewGreeterClient`.
package goclient

import (
	"context"

	pb "example.com/helloworld"
	"google.golang.org/grpc"
)

func Hello(conn *grpc.ClientConn) {
	pb.NewGreeterClient(conn).SayHello(context.Background(), &pb.HelloRequest{Name: "go"}) // inferred -> server.SayHello
}

func Hi(conn *grpc.ClientConn) {
	pb.NewGreeterClient(conn).SayHi(context.Background(), &pb.HelloRequest{Name: "go"}) // inferred -> GreeterServer.sayHiHandler
}

func Local(conn *grpc.ClientConn) {
	NewThing(conn).SayHello() // negative control: not a generated client factory
}
