/* Fixture (P5, napi): a second addon also exporting `hello` -> ambiguous. */
#include <node_api.h>

static napi_value HelloAgain(napi_env env, napi_callback_info info) {
    return NULL;
}

static napi_value Init2(napi_env env, napi_value exports) {
    napi_property_descriptor desc = DECLARE_NAPI_METHOD("hello", HelloAgain);
    napi_define_properties(env, exports, 1, &desc);
    return exports;
}
