/* Fixture (bridges, expanded export): the C side declares the exported function. */
int add_numbers(int a, int b);
int private_helper(void); /* negative control: not exported */
