// Fixture (round 2, http mounts): browser client of the mounted route.
export async function loadItem() {
  return fetch("/api/items/1"); // one distinct mount prefix -> inferred
}
