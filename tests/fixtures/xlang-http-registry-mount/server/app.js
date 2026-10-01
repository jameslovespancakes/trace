// Fixture (registry registered as a handler): the routes of `router` are served under the
// prefix `app.use` registers it at.
const app = makeApp();
const router = makeRouter();

function health(req, res) {
  res.end("ok");
}

router.get("/health", health);
app.use("/ops", router);

module.exports = app;
