// Fixture (bridges gate, angular-httpclient family): the Angular HttpClient.
import { HttpClient } from "@angular/common/http";

export class UsersService {
  constructor(private http: HttpClient) {}

  load(id: string) {
    return this.http.get(`/nest/users/${id}`);
  }
}
