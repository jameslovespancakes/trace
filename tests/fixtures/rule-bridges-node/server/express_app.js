// Fixture (bridges gate, express family): a route, a router and its mount.
const express = require("express");

const app = express();
const router = express.Router();

function getUser(req, res) {
  res.json({ id: req.params.id });
}

function health(req, res) {
  res.json({ ok: true });
}

app.get("/express/users/:id", getUser);
router.get("/health", health);
app.use("/ops", router);

module.exports = app;
