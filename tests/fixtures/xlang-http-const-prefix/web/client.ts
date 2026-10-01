// Fixture (round 2, http constant prefix): browser client of the mounted routes.
export async function loadItem() {
  return fetch("/api/v1/items/1"); // prefix resolved from settings.API_V1_STR -> inferred
}

export async function loadUser() {
  return fetch("/api/v1/users/1"); // prefix unknown (bound twice) -> possible, suffix match
}
