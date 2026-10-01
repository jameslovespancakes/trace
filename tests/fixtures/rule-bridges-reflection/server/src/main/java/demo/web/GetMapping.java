// Fixture (bridges, reflection): a composed annotation declared in available source; its
// meta-annotation chain reaches the request-mapping root row.
package demo.web;

import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;

@Retention(RetentionPolicy.RUNTIME)
@RequestMapping(method = RequestMethod.GET)
public @interface GetMapping {
    @AliasFor(annotation = RequestMapping.class)
    String[] value() default {};

    String name() default "";
}
