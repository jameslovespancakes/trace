// Fixture (bridges gate, restify family): a route.
const restify = require("restify");

const server = restify.createServer();

function restifyPing(req, res, next) {
  res.send("pong");
  return next();
}

server.get("/restify/ping", restifyPing);

module.exports = server;
