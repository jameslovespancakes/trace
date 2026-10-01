// Fixture (bridges gate, echo family, new coverage): a route.
package server

import "github.com/labstack/echo/v4"

func echoBook(c echo.Context) error { return nil }

func Echo() *echo.Echo {
	e := echo.New()
	e.GET("/echo/books/:id", echoBook)
	return e
}
