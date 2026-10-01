/* Fixture (P5, cpython): a method table without a PyModuleDef in this file (module unknown). */
#include <Python.h>

static PyObject *ham_shared(PyObject *self, PyObject *args) {
    return NULL;
}

static PyMethodDef HamMethods[] = {
    {"shared", (PyCFunction)ham_shared, METH_NOARGS, NULL},
    {NULL, NULL, 0, NULL}
};
