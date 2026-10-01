def test_decorator_registers(app):
    @app.get("/")
    def index():
        return "ok"

    assert app.routes[("GET", "/")] is index


def test_call_registers(app):
    register = app.get("/items")
    assert callable(register)
