import { Context } from './context'

export type Handler = (c: Context) => Response | Promise<Response>

export class Hono {
  #routes: [string, Handler][] = []

  get = (path: string, handler: Handler): Hono => {
    this.#routes.push([path, handler])
    return this
  }

  fetch = async (path: string): Promise<Response> => {
    const c = new Context()
    for (const [p, handler] of this.#routes) {
      if (p === path) {
        return handler(c)
      }
    }
    return c.notFound()
  }
}

export { Context } from './context'
