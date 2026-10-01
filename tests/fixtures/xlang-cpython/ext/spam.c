/* Fixture (P5, cpython): the classic `spam` extension module. */
#include <Python.h>

static PyObject *spam_system(PyObject *self, PyObject *args) {
    return NULL;
}

static PyMethodDef SpamMethods[] = {
    {"system", spam_system, METH_VARARGS, "Execute a shell command."},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef spammodule = {
    PyModuleDef_HEAD_INIT,
    "spam",
    NULL,
    -1,
    SpamMethods
};

PyMODINIT_FUNC PyInit_spam(void) {
    return PyModule_Create(&spammodule);
}
