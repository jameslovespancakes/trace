class JSONProvider:
    def dumps(self, obj: object) -> str:
        return str(obj)


class App:
    json: JSONProvider

    def __init__(self) -> None:
        self.json = JSONProvider()
        self.policies: dict[str, object] = {}

    def redirect(self, location: str, code: int = 302) -> str:
        """Create a redirect response object (flask sansio/app.py)."""
        return f"{code} {location}"

    def create_jinja_environment(self) -> dict[str, object]:
        # A method passed as a value: a non-call reference to JSONProvider.dumps.
        self.policies["json.dumps_function"] = self.json.dumps
        return self.policies
