// Fixture (bridges gate, nestjs family, new coverage): decorator-registered routes.
import { Controller, Get, Param } from "@nestjs/common";

@Controller("nest/users")
export class UsersController {
  @Get(":id")
  findOne(@Param("id") id: string) {
    return { id };
  }
}
