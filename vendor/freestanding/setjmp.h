#pragma once

// libvpx reports decode errors by longjmp'ing back to a setjmp in the decoder
typedef void *jmp_buf[5];
#define setjmp(buf) __builtin_setjmp(buf)
// __builtin_longjmp only accepts 1
#define longjmp(buf, val) __builtin_longjmp(buf, 1)
