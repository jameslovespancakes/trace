"""Fixture (P5, grpc): Python client using the generated stub."""
import grpc

import helloworld_pb2
import helloworld_pb2_grpc


def run():
    with grpc.insecure_channel("localhost:50051") as channel:
        stub = helloworld_pb2_grpc.GreeterStub(channel)
        stub.SayHello(helloworld_pb2.HelloRequest(name="you"))  # inferred -> server.SayHello
        stub.SayGoodbye(helloworld_pb2.HelloRequest(name="you"))  # two servers -> possible
        stub.NotAnRpc()  # negative control: no rpc, no server method


def say_hello_locally():
    """Negative control: same method name, no stub."""
    return "SayHello"
