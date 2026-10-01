// Fixture (typed dispatch rule): a router whose handlers are reached only through declared
// types - the value flow through the route table is not followed.
package router

import "net/http"

// HandlerFunc values are called by the entry (ServeHTTP -> Next).
type HandlerFunc func(*Context)

// Chain is a named sequence of handlers.
type Chain []HandlerFunc

// MiddlewareFunc composes handlers (called by the entry too, but over handler types).
type MiddlewareFunc func(next HandlerFunc) HandlerFunc

// Middlewares is a named sequence of middleware.
type Middlewares []MiddlewareFunc

// Hook values are called outside the entry-reachable code only.
type Hook func()

// Func implements the entry protocol itself (its ServeHTTP calls it).
type Func func(http.ResponseWriter, *http.Request)

func (f Func) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	f(w, r)
}

type Context struct {
	handlers Chain
	index    int
}

func (c *Context) Next() {
	for c.index < len(c.handlers) {
		c.handlers[c.index](c)
		c.index++
	}
}

type Router struct {
	prefix string
	routes map[string]Chain
	mw     Middlewares
	hook   Hook
}

func (e *Router) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	c := &Context{}
	c.handlers = e.routes[r.URL.Path]
	e.mw[0](nil)
	c.Next()
}

func (e *Router) Start() {
	e.hook()
}

func (e *Router) handle(method, path string, handlers Chain) {
	e.routes[method+path] = handlers
}

// GET registers under its path with the verb its name spells.
func (e *Router) GET(path string, handlers ...HandlerFunc) {
	e.handle("GET", e.prefix+path, handlers)
}

// Handle: the key is the nearest string parameter before the handlers.
func (e *Router) Handle(method, path string, handlers ...HandlerFunc) {
	e.handle(method, e.prefix+path, handlers)
}

// Use takes middleware only: no registration.
func (e *Router) Use(prefix string, m ...MiddlewareFunc) {
	e.mw = append(e.mw, m...)
}

// Mount takes the entry protocol's interface.
func (e *Router) Mount(pattern string, h http.Handler) {
	e.routes[pattern] = Chain{}
}

// HandleFunc takes a type implementing the entry protocol.
func (e *Router) HandleFunc(pattern string, f Func) {
	e.routes[pattern] = Chain{}
}

// OnStart takes a function type the entry never calls.
func (e *Router) OnStart(name string, h Hook) {
	e.hook = h
}

// Group builds a registry whose prefix field (used in its registration keys) holds the
// parameter: routes of the result are under it; its handlers are middleware.
func (e *Router) Group(prefix string, handlers ...HandlerFunc) *Router {
	return &Router{prefix: e.prefix + prefix}
}

// register is not exported and has several string parameters (internal ones): nothing.
func (e *Router) register(host, name string, h HandlerFunc) {
	e.routes[host+name] = Chain{h}
}

// add is not exported but its key is its only string parameter: it registers (callers
// compose it).
func (e *Router) add(pattern string, h HandlerFunc) {
	e.routes[pattern] = Chain{h}
}

// Last is a method of a library-defined sequence type: library code.
func (c Chain) Last() HandlerFunc {
	return c[len(c)-1]
}

// Final calls a method of a library-defined type on its parameter: no method of the
// caller's object runs.
func (e *Router) Final(hs Chain) HandlerFunc {
	return hs.Last()
}

// A second entry protocol of the test's table, declared here: Server.Serve(w, r).
type Server interface {
	Serve(w Writer, r *Request)
}

type Writer struct{}

type Request struct{}

// HandleServe takes an anonymous function type with the protocol method's signature.
func (e *Router) HandleServe(pattern string, f func(Writer, *Request)) {
	e.routes[pattern] = Chain{}
}

// HandleWriter takes an anonymous function type of another signature: data.
func (e *Router) HandleWriter(pattern string, f func(Writer)) {
	e.routes[pattern] = Chain{}
}
