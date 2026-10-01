// Fixture (bridges gate, next family): a filesystem-routed API handler.
export default function orderHandler(req, res) {
  res.status(200).json({ id: req.query.id });
}
