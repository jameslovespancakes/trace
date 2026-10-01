// Fixture (P5, napi): negative control with the same function name as the addon export.
function sumValues(a, b) {
  return a + b;
}

module.exports = { sumValues };
