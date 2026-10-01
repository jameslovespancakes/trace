// Fixture (bridges gate, gin family): a route on a router group.
package server

import "github.com/gin-gonic/gin"

func ginUser(c *gin.Context) {}

func Gin() *gin.Engine {
	r := gin.Default()
	r.GET("/gin/users/:id", ginUser)
	return r
}
