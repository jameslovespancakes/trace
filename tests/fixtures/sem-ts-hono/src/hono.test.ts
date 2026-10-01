import { Context, Hono } from './hono'

// Minimal test harness (callbacks, like vitest's describe/it).
function describe(name: string, fn: () => void): void {
  fn()
}
function it(name: string, fn: () => void | Promise<void>): void {
  void fn()
}

// Module-level registrations with anonymous callbacks (hono.test.ts, client.test.ts).
const app = new Hono()
app.get('/notfound', (c) => c.notFound())
app.get('/redirect', (c) => {
  return c.redirect('/empty', 301)
})

// A module-level call outside any function.
const direct = new Context().notFound()

describe('Context', () => {
  const c = new Context()
  it('c.redirect()', async () => {
    let res = c.redirect('/destination')
    res = c.redirect('https://example.com/destination')
    void res
  })
  it('c.notFound()', () => {
    const res = c.notFound()
    void res
  })
})

// Non-call uses: a read (method passed as a value) and a write.
const handler = new Context().notFound
new Context().redirect = (location: string) => new Response(location)
void direct
void handler
