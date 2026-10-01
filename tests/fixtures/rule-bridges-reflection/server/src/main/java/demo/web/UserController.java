// Fixture (bridges, reflection): a controller whose class and method annotations give the route.
package demo.web;

@RequestMapping("/api")
public class UserController {
    @GetMapping("/users/{id}")
    public String get(String id) {
        return id;
    }

    public String notARoute(String id) {
        return id;
    }
}
