// Fixture (bridges gate, fastify family): a route shorthand and the route options form.
const fastify = require("fastify")();

async function listOrders() {
  return [];
}

async function createOrder() {
  return {};
}

fastify.get("/fastify/orders", listOrders);
fastify.route({ method: "POST", url: "/fastify/orders", handler: createOrder });

module.exports = fastify;
