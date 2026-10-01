// Fixture (bridges gate, spring family): request-mapping annotations.
package demo;

import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RequestMethod;

@RequestMapping("/spring")
public class SpringController {
    @RequestMapping(value = "/orders/{id}", method = RequestMethod.GET)
    public String order(String id) {
        return id;
    }

    @GetMapping("/users/{id}")
    public String user(String id) {
        return id;
    }
}
