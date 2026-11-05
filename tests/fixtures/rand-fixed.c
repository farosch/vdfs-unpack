/*
 * Deterministic replacement for OpenSSL's RAND_bytes, used only to build the
 * encrypted fixture. mkfs draws each descriptor's AES nonce from RAND_bytes.
 * The call is internal to the statically linked mkfs, so LD_PRELOAD cannot
 * intercept it. Link with -Wl,--wrap=RAND_bytes and this object instead.
 */
int __real_RAND_bytes(unsigned char *buf, int num);

int __wrap_RAND_bytes(unsigned char *buf, int num)
{
	int i;

	(void)__real_RAND_bytes;
	for (i = 0; i < num; i++)
		buf[i] = (unsigned char)(0xA5 ^ i);
	return 1;
}
