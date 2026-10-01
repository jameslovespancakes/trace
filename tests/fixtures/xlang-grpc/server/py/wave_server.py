"""Fixture (round 2, grpc): an abstract servicer base whose concrete subclasses implement the
rpc (chosen at run time): both are candidates -> possible."""
import helloworld_pb2_grpc


class BaseWaver(helloworld_pb2_grpc.GreeterServicer):
    def __init__(self, name):
        self.name = name


class LoudWaver(BaseWaver):
    def Wave(self, request, context):
        return "WAVE"


class QuietWaver(BaseWaver):
    def Wave(self, request, context):
        return "wave"


class Unrelated:
    def Wave(self, request, context):
        """Negative control: same method name, not a servicer."""
        return None
