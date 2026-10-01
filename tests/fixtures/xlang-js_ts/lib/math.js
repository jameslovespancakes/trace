// Fixture (P5, js_ts): JavaScript implementation described by math.d.ts.
function add(a, b) {
  return a + b;
}

// Ambiguous: `sub` is defined twice (the later one wins at runtime; both are candidates).
function sub(a, b) {
  return a - b;
}

function sub(a, b) {
  return b - a;
}

class Calc {
  mul(a, b) {
    return a * b;
  }
}

module.exports = { add, sub, Calc };
