// hono-shaped fixture (src/context.ts): methods are class-field arrow functions.
export type RedirectStatusCode = 301 | 302 | 303 | 307 | 308

export class Context {
  res: Response | undefined
  #notFoundHandler: ((c: Context) => Response) | undefined

  header = (name: string, value: string): void => {
    this.res?.headers.set(name, value)
  }

  newResponse = (body: string | null, status: number): Response => {
    return new Response(body, { status })
  }

  /**
   * `.redirect()` can Redirect, default status code is 302.
   */
  redirect = <T extends RedirectStatusCode = 302>(location: string | URL, status?: T): Response => {
    const locationString = String(location)
    this.header('Location', locationString)
    return this.newResponse(null, status ?? 302)
  }

  /**
   * `.notFound()` can return the Not Found Response.
   */
  notFound = (): Response => {
    this.#notFoundHandler ??= () => new Response('404 Not Found', { status: 404 })
    return this.#notFoundHandler(this)
  }
}
