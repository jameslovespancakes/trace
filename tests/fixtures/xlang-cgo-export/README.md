# xlang-cgo-export

cgo `//export name` (round 3): a Go function preceded by the directive `//export name` is the
C symbol `name`; C code declares it `extern` and calls it.

| C prototype (c/unlock.c) | expected |
|---|---|
| `wait_for_unlock` | proven `cgo` bridge to `go/notify.go:waitForUnlock` |
| `not_exported` | none (`// export not_exported` is not the directive: a space after `//`) |
| `dup_symbol` | possible: the Go export and the C definition in `c/dup.c` compete |

Test: `trace-bridge` `fixture_cgo_export`.
