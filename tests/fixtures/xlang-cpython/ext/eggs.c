/* Fixture (P5, cpython): module `eggs`, shadowed by py/eggs.py (negative control). */
#include <Python.h>

static PyObject *eggs_lay(PyObject *self, PyObject *args) {
    return NULL;
}

static PyMethodDef EggsMethods[] = {
    {"lay", eggs_lay, METH_NOARGS, NULL},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef eggsmodule = {PyModuleDef_HEAD_INIT, "eggs", NULL, -1, EggsMethods};
