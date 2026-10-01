// Fixture (P5, napi): JavaScript loading the compiled addon.
const addon = require("../addon/addon.node");
const util = require("./util");

function main() {
  const s = addon.sumValues(1, 2); // proven -> sum_values
  const h = addon.hello(); // two exports named `hello` -> possible
  addon.unknownFn(); // negative control: not exported
  util.sumValues(3, 4); // negative control: plain JavaScript module
  return s + h;
}

module.exports = { main };
