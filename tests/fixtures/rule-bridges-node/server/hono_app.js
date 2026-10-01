// Fixture (bridges gate, hono family, new coverage): a route.
const { Hono } = require("hono");

const app = new Hono();

function honoBook(c) {
  return c.json({ id: c.req.param("id") });
}

app.get("/hono/books/:id", honoBook);

module.exports = app;
