// Fixture (bridges gate, go net/http client family).
package client

import "net/http"

func Load() (*http.Response, error) {
	return http.Get("http://svc.local/gin/users/1")
}
