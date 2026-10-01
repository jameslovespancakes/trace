import flask
from flask.sansio.app import App

# Module-level code: a call and a read owned by <module>.
default_app = App()
default_app.redirect("/start")
handler = default_app.redirect


def test_redirect_with_app(app: App) -> None:
    def redirect(location: str, code: int = 302) -> str:
        raise ValueError

    # A write to the method attribute (flask tests/test_helpers.py:174).
    app.redirect = redirect

    flask.redirect("other")
