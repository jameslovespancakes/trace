// Fixture (registry registered as a handler): client of the mounted route.
async function loadHealth() {
  return fetch("/ops/health");
}

module.exports = { loadHealth };
