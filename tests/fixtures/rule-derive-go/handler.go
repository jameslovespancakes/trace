// Library-to-your-code callbacks (rule: a method called on a parameter, or on the field it
// was stored in, whose declared type is an interface runs on the object the caller passed).
package web

type Handler interface {
	ServeHTTP(w ResponseWriter, r *Request)
}

type ResponseWriter interface {
	Write(b []byte) (int, error)
}

type Request struct {
	Method string
}

func (r *Request) Reset() {}

type Server struct {
	Addr    string
	Handler Handler
}

type serverHandler struct {
	srv *Server
}

func (sh serverHandler) ServeHTTP(rw ResponseWriter, req *Request) {
	handler := sh.srv.Handler
	handler.ServeHTTP(rw, req)
}

func (srv *Server) Serve() {
	serverHandler{srv}.ServeHTTP(nil, nil)
}

func ListenAndServe(addr string, handler Handler) error {
	server := &Server{Addr: addr, Handler: handler}
	server.Serve()
	return nil
}

func Dispatch(handler Handler, req *Request) {
	handler.ServeHTTP(nil, req)
}

func Reset(req *Request) *Request {
	req.Reset()
	return req
}
