// Fixture (group prefixes): routes registered on a group of a group are under both
// prefixes (the library derives `Group(prefix)` as the result's own prefix).
package server

func userHandler(c *Context) {}

func Routes(r *Engine) {
	v1 := r.Group("/api/v1")
	admin := v1.Group("/admin")
	admin.GET("/users/:id", userHandler)
}
