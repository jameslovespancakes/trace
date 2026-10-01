from collections.abc import Callable, Iterable, Mapping
from typing import Any, TypeVar, overload

_T = TypeVar("_T")
_F = TypeVar("_F", bound=Callable[..., Any])
_HANDLER = Callable[[int, object], Any] | int | None

class Thread:
    def __init__(
        self,
        group: None = None,
        target: Callable[..., object] | None = None,
        name: str | None = None,
        args: Iterable[Any] = (),
        kwargs: Mapping[str, Any] | None = None,
        *,
        daemon: bool | None = None,
    ) -> None: ...
    def start(self) -> None: ...

def signal(signalnum: int, handler: _HANDLER, /) -> _HANDLER: ...
def print(*values: object, sep: str | None = " ", end: str | None = "\n") -> None: ...
@overload
def sorted(iterable: Iterable[_T], /, *, key: None = None, reverse: bool = False) -> list[_T]: ...
@overload
def sorted(iterable: Iterable[_T], /, *, key: Callable[[_T], Any], reverse: bool = False) -> list[_T]: ...
def register(func: _F, /, *args: Any, **kwargs: Any) -> _F: ...
