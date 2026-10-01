// Fixture (bridges, fs routes): the client of the filesystem route.
export async function loadUser() {
  return fetch("/api/users/7");
}
