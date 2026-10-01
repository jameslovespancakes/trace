/* Fixture (round 3, cgo //export): C calling Go through cgo-exported entry points. */
extern int wait_for_unlock(void *db);
extern int not_exported(void);
extern int dup_symbol(void);

int step(void *db) {
    int rv = wait_for_unlock(db);
    rv += not_exported();
    rv += dup_symbol();
    return rv;
}
