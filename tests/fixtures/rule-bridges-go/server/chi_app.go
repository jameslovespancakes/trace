// Fixture (bridges gate, chi family): a route.
package server

import (
	"net/http"

	"github.com/go-chi/chi/v5"
)

func chiItem(w http.ResponseWriter, r *http.Request) {}

func Chi() *chi.Mux {
	r := chi.NewRouter()
	r.Get("/chi/items/{id}", chiItem)
	return r
}
