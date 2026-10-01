// Fixture (round 2, grpc): Node server whose handler table binds a static method
// (`sayHi: GreeterServer.sayHiHandler.bind(this)`): the handler is the implementation.
const grpc = require("@grpc/grpc-js");

class GreeterServer {
  constructor(proto) {
    this.server = new grpc.Server();
    this.register(proto);
  }

  static sayHiHandler(call, callback) {
    callback(null, { message: "hi " + call.request.name });
  }

  register(proto) {
    this.server.addService(proto.helloworld.Greeter.service, {
      sayHi: GreeterServer.sayHiHandler.bind(this),
    });
  }
}

module.exports = GreeterServer;
