var res = module.exports = {};

res.redirect = function redirect(url) {
  this.location = url;
  return this;
};

res.send = function send(body) {
  if (body === undefined) {
    return this.redirect("/");
  }
  return body;
};
