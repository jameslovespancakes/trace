// Fixture (bridges gate, koa family): a koa router route.
const Koa = require("koa");
const Router = require("@koa/router");

const app = new Koa();
const router = new Router();

async function koaItem(ctx) {
  ctx.body = ctx.params.id;
}

router.get("/koa/items/:id", koaItem);
app.use(router.routes());

module.exports = app;
