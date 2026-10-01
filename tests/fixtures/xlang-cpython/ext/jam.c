/* Fixture (P5, cpython): another table exporting `shared` with designated initializers. */
#include <Python.h>

static PyObject *jam_shared(PyObject *self, PyObject *args) {
    return NULL;
}

static PyMethodDef JamMethods[] = {
    {.ml_name = "shared", .ml_meth = jam_shared, .ml_flags = METH_NOARGS},
    {NULL, NULL, 0, NULL}
};
