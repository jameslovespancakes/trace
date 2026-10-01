// Fixture (bridges gate, node HTTP client families).
const axios = require("axios");
const got = require("got");
const superagent = require("superagent");

async function withFetch() {
  return fetch("/express/users/7");
}

async function withAxios() {
  return axios.post("/fastify/orders", {});
}

async function withGot() {
  return got.get("http://svc.local/koa/items/1");
}

async function withSuperagent() {
  return superagent.get("http://svc.local/hono/books/2");
}

module.exports = { withFetch, withAxios, withGot, withSuperagent };
