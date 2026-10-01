// Fixture (bridges, fs routes): a filesystem-routed API handler (default export).
export default function handler(req: unknown, res: unknown) {
  return { req, res };
}
