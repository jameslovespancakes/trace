// Fixture (bridges gate, jaxrs family): path and method-designator annotations.
package demo;

import jakarta.ws.rs.GET;
import jakarta.ws.rs.Path;

@Path("/jaxrs")
public class JaxrsResource {
    @GET
    @Path("/items/{id}")
    public String item(String id) {
        return id;
    }
}
