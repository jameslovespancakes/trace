import { handler, Handler } from "./handlers";
import * as ns from "./handlers";

export class App {
  handler: Handler = handler;

  run(e: Error): string {
    return this.handler(e);
  }
}

export function viaApp(app: App, e: Error): string {
  return app.handler(e);
}

export function direct(e: Error): string {
  return ns.handler(e);
}
