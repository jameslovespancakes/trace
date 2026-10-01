from .sansio.app import App

current_app = App()


def redirect(location: str, code: int = 302) -> str:
    """Redirect through the current app (flask helpers.py)."""
    return current_app.redirect(location, code)
