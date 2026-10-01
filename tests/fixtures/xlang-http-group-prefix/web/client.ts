// Fixture (group prefixes): the client names the full path.
export async function loadUser(): Promise<Response> {
  return fetch("/api/v1/admin/users/7");
}
