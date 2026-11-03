#pragma once

// libvpx reports decode errors by longjmp'ing back to a setjmp in the decoder
typedef void *jmp_buf[5];

// HACK: on Windows x64, clang saves the frame's base instead of rbp, which
// longjmp then restores into rbp. We overwrite it with the real rbp after the
// first return, which is what the resumed code expects.
#define setjmp(buf)                                                            \
  (__builtin_setjmp(buf) ? 1 : ({                                              \
    __asm__ volatile("mov %%rbp, %0" : "=m"((buf)[0]));                        \
    0;                                                                         \
  }))
// __builtin_longjmp only accepts 1
#define longjmp(buf, val) __builtin_longjmp(buf, 1)
