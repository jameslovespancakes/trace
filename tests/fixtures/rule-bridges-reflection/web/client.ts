// Fixture (bridges, reflection): the client of the annotated route.
export async function loadUser() {
  return fetch("/api/users/7");
}
