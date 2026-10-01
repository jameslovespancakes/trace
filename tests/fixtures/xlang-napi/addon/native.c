/* Fixture (P5, napi): C N-API addon registering `hello`. */
#include <node_api.h>

static napi_value Hello(napi_env env, napi_callback_info info) {
    return NULL;
}

static napi_value Init(napi_env env, napi_value exports) {
    napi_value fn;
    napi_create_function(env, "hello", NAPI_AUTO_LENGTH, Hello, NULL, &fn);
    napi_set_named_property(env, exports, "hello", fn);
    return exports;
}
