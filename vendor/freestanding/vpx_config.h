/*
 * UEFI bare metal for libvpx config.
 *
 * Overrides the `CONFIG_OS_SUPPORT` boolean in the generated config.
 */

#include_next <vpx_config.h>

/*
 * UEFI has no OS APIs (no pthreads, clock_gettime, etc.)
 *
 * NOTE: While configure has a `none` case, that only applies to ARM
 * architectures, therefore this needs to be manually defined here.
 */
#undef CONFIG_OS_SUPPORT
#define CONFIG_OS_SUPPORT 0
