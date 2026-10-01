import type { Context } from '../context'

type Next = () => Promise<void>

// Middleware factories return anonymous async functions (hono trailing-slash/index.ts).
export const trimTrailingSlash = () => {
  return async function trimTrailingSlash2(c: Context, next: Next) {
    await next()
    if (c.res?.status === 404) {
      c.res = c.redirect('/trimmed', 301)
    }
  }
}

export const appendTrailingSlash = () => {
  return async (c: Context, next: Next) => {
    await next()
    if (c.res?.status === 404) {
      c.res = c.redirect('/appended/', 301)
    }
  }
}
