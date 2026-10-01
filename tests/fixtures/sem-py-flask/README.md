# sem-py-flask (trace-semantic fixture; never imported or run)

flask-shaped non-call uses and module-level code for Pyright:

| target | uses |
|---|---|
| `src/flask/sansio/app.py` `App.redirect` | call in `helpers.redirect`; module-level call and read in `tests/test_helpers.py` (owned by `<module>`); **write** `app.redirect = redirect` in `test_redirect_with_app` |
| `src/flask/sansio/app.py` `JSONProvider.dumps` | read `self.json.dumps` passed as a value (flask app.py:507) |

In flask itself the test parameter `app` is an unannotated pytest fixture, so Pyright has no
type for it; here it is annotated (`app: App`) to exercise the mechanism. The unannotated case
stays a name match (`possible`) for the completeness report (P4).
