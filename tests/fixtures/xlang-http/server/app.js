// Fixture (P5, http): Express server.
const express = require("express");

const app = express();
const router = express.Router();

function getUser(req, res) {
  res.json({ id: req.params.id });
}

function status(req, res) {
  res.json({ ok: true });
}

app.get("/users/:id", getUser); // also served by server/api.py -> possible
app.post("/users", (req, res) => res.status(201).end()); // unique -> inferred
router.get("/status", status); // mounted under /api below -> inferred for /api/status
app.use("/api", router);

module.exports = app;
